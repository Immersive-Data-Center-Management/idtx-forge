//! `StageAccess` — the host-side bridge from guest imports to the live
//! ECS `World` (§4.2).
//!
//! Guest imports (xform get/set, scene queries, XR pose reads) resolve
//! through this façade rather than touching USD on the hot path. The host
//! installs a borrow for the duration of each hook via [`WorldScope`];
//! imports read it back through the [`wasmtime::Caller`]'s [`HostState`].
//!
//! # Safety model
//!
//! WASM guests are single-threaded and the host dispatches hooks
//! sequentially, one entity at a time, holding a `&mut World` for the
//! whole sweep. We stash a raw `*mut World` in [`HostState`] guarded by a
//! [`WorldScope`] RAII guard that clears it when the hook returns, so no
//! import can dereference a dangling world. Access goes through the
//! checked accessors below, which validate the pointer is set before use.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bevy_ecs::entity::Entity as EcsEntity;
use bevy_ecs::world::World;
use idtx_behavior_abi::{
    ArcCastParams, ArcHitResult, CameraPose, Entity as WireEntity, ErrorCode, InputActionId,
    InputPhase, PlaybackCommandKind, RayHitResult, RaycastParams, WidgetEventKind, Xform,
    XrPoseResult, XrSpace,
};

use crate::ecs as ec;
use crate::events::{Event, EventQueue};
use crate::input::InputSnapshot;
use crate::level_transition::LevelTransitionQueue;
use crate::manifest::Capabilities;
use crate::overlay::{OverlayOp, OverlayQueue};
use crate::physics_control::PhysicsControlQueue;
use crate::playback::PlaybackQueue;
use crate::scene_backend::SceneBackend;
use crate::session_edit::{SessionEditOp, SessionEditQueue};
use crate::xr_snapshot::XrSnapshot;

/// A completed asynchronous HTTP request (`net` capability), produced off the
/// hot path by a background worker and delivered to the guest on a later frame
/// via `_idtx_on_http_response`.
///
/// A `status` of `0` means the request never completed at the transport level
/// (DNS / connect / TLS / timeout) **or** was denied by the SSRF policy — the
/// same convention the synchronous `idtx_http_request` path uses. The `body` is
/// already capped to the policy's `max_response_bytes`.
#[derive(Debug, Clone)]
pub struct HttpCompletion {
    /// The guest-chosen correlation id echoed back so the SDK can route the
    /// completion to the handler registered for this request.
    pub request_id: u32,
    /// The HTTP status code, or `0` on a transport failure / policy denial.
    pub status: u16,
    /// The response body (already capped to the policy limit).
    pub body: Vec<u8>,
}

impl crate::work::RetainedSize for HttpCompletion {
    fn retained_size(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(self.body.capacity())
    }
}

/// Bounded async completion inbox. Accepted workers reserve their slot before
/// spawning; injected completions use the same queue limits.
#[derive(Debug)]
pub struct HttpInbox {
    pending: crate::work::BoundedQueue<HttpCompletion>,
}

impl HttpInbox {
    fn new(budget: crate::work::QueueBudget, max_body: usize) -> Self {
        Self {
            pending: crate::work::BoundedQueue::new(budget, max_body),
        }
    }

    pub fn push(&mut self, value: HttpCompletion) -> Result<(), ErrorCode> {
        self.pending.push(value)
    }

    #[cfg(feature = "net")]
    pub(crate) fn push_reserved(
        &mut self,
        value: HttpCompletion,
        reservation: crate::work::Reservation,
    ) -> Result<(), ErrorCode> {
        self.pending.push_reserved(value, reservation)
    }

    pub(crate) fn pop(&mut self) -> Option<crate::work::Queued<HttpCompletion>> {
        self.pending.pop()
    }
    pub(crate) fn return_front(&mut self, value: crate::work::Queued<HttpCompletion>) {
        self.pending.return_front(value);
    }
    fn clear(&mut self) {
        self.pending.clear();
    }
}

static NEXT_SPAWN_TICKET: AtomicU64 = AtomicU64::new(1);
const MAX_PENDING_SPAWN_TICKETS: usize = 64;

struct SpawnRecord {
    result: idtx_behavior_abi::SpawnResult,
    _reservation: crate::work::Reservation,
}

/// Per-store host state carried by the wasmtime `Store<HostState>`.
pub struct HostState {
    /// Immutable context captured before instantiation and attachment hooks.
    pub(crate) behavior_context: Option<String>,
    pub(crate) spawn_generation: u64,
    /// Last SDK-reported authored hook error, reset before every dispatch.
    pub(crate) hook_error: Option<ErrorCode>,
    spawn_records: std::collections::BTreeMap<u64, SpawnRecord>,
    /// Transient per-instance input values. Never projected into USD, logs or
    /// experience state; access is confined to the owning widget behavior.
    pub(crate) widget_values: std::collections::BTreeMap<String, String>,
    pub experience_state: crate::ExperienceState,
    pub cancel_level_transition: bool,
    pub(crate) auth_scopes: idtx_auth::AuthScopes,
    pub(crate) queue_budget: crate::work::QueueBudget,
    pub(crate) work_limits: crate::limits::WorkLimits,
    /// Granted capabilities for the owning behavior.
    pub capabilities: Capabilities,
    /// The entity the behavior is attached to
    /// ([`WireEntity::NULL`] for a pseudo-root behavior).
    pub self_entity: WireEntity,
    /// Frame delta-time (seconds) for the current dispatch.
    pub delta_seconds: f32,
    /// Stage time-code for the current dispatch.
    pub time_code: f64,
    /// Resolved input snapshot for the current frame.
    pub input: InputSnapshot,
    /// Most recently polled XR tracked-space poses for the current frame.
    pub xr_snapshot: XrSnapshot,
    /// The play-space recenter offset for the current frame — a stage-space
    /// ground translation (y = 0) a pawn-root behavior adds to its authored
    /// translation (`xr_hmd` capability). The compositor subtracts the same
    /// offset from every tracked pose, so the view stays fixed while the body
    /// re-bases under the headset. `ZERO` when no recenter is active.
    pub xr_recenter: glam::Vec3,
    /// The active view/player camera pose for the current frame (desktop
    /// orbit/player camera, or the HMD in XR). Seeded by the session before each
    /// frame and read by the `idtx_camera_pose` import. `valid == 0` when none.
    pub camera_pose: CameraPose,
    /// The active view/player camera's full world transform (stage space) for
    /// the current frame. Seeded beside [`Self::camera_pose`] and read by the
    /// `idtx_camera_transform` import. `IDENTITY` when no camera this frame.
    pub camera_transform: glam::Mat4,
    /// Cached action-name → id resolution.
    pub action_ids: HashMap<String, u32>,
    /// Pending events posted by the guest this frame.
    pub events: EventQueue,
    /// Pending playback-transport commands posted by the guest this frame.
    pub playback: PlaybackQueue,
    /// Pending physics-control commands posted by the guest this frame (grab
    /// suspend/resume). Drained by the session after the update hooks and
    /// applied to its physics subsystem before the physics tick; the host does
    /// not know what physics is — it only buffers `(entity, simulated)`.
    pub physics_control: PhysicsControlQueue,
    /// Pending deferred session-layer edits posted by the guest this frame
    /// Drained by the session after the update hooks, applied to the
    /// session layer, and re-projected the same frame.
    pub session_edits: SessionEditQueue,
    /// Pending deferred level-transition requests posted by the guest this frame.
    /// Drained by the session after the update hooks and
    /// surfaced to the runtime driver, which performs the actual session
    /// swap; the host only buffers the `(target, spawn)` — it has no knowledge of
    /// levels or sessions.
    pub level_transitions: LevelTransitionQueue,
    /// Pending transient overlay / render commands posted by the guest this
    /// frame. Drained by the render side after the update hooks and fed
    /// into the overlay pass / post-process chain; never authored into USD.
    pub overlay: OverlayQueue,
    /// The event currently being delivered to `on_event`, seeded by the
    /// dispatcher immediately before the hook fires and cleared after. The
    /// `idtx_event_current_*` imports read it back; `None` outside an
    /// `on_event` dispatch.
    current_event: Option<Event>,
    /// wasmtime resource limiter (linear-memory cap).
    pub limits: wasmtime::StoreLimits,
    /// Scene-query backend (raycast / arc-cast / prim-type query), installed by
    /// the session via [`BehaviorHost::set_scene_backend`](crate::BehaviorHost).
    /// `None` in headless hosts — the scene-query imports then return
    /// [`ErrorCode::NotFound`] / `hit == 0`.
    pub scene_backend: Option<Arc<dyn SceneBackend>>,
    /// Network-egress policy for the `net` capability (the SAME deny-by-default
    /// SSRF gate that guards `OpenExec` REST datasources). Installed by the session
    /// via [`BehaviorHost::set_network_policy`](crate::BehaviorHost) from its
    /// `[network]` config; defaults to [`idtx_net_guard::NetworkPolicy::denied`],
    /// so a `net` request is rejected unless the runtime explicitly allows the
    /// host. The type is always present; the actual HTTP (behind the crate's
    /// `net` feature) reads it before every request.
    pub network_policy: idtx_net_guard::NetworkPolicy,
    /// Inbox for completed asynchronous HTTP requests (`net` capability).
    /// Background workers spawned by `idtx_http_request_async` push a
    /// [`HttpCompletion`] here when the request finishes; the host drains it at
    /// the top of each [`BehaviorHost::run_frame`](crate::BehaviorHost) and
    /// delivers each completion to the guest via `_idtx_on_http_response`. The
    /// `Arc` is cloned into each worker, so a completion pushed after the
    /// instance detaches is harmlessly dropped with the queue.
    pub http_inbox: Arc<Mutex<HttpInbox>>,
    /// Count of in-flight asynchronous HTTP requests for this instance, shared
    /// with the background workers (incremented when a request is accepted,
    /// decremented when its worker finishes). `idtx_http_request_async` rejects a
    /// new request once this reaches the host cap, bounding thread growth from a
    /// behavior that fires a request every frame.
    pub http_outstanding: Arc<AtomicUsize>,
    /// The HTTP completion currently being delivered to `_idtx_on_http_response`,
    /// seeded by the dispatcher immediately before the hook fires and cleared
    /// after. The `idtx_http_response_*` imports read it back; `None` outside the
    /// callback. Mirrors [`Self::current_event`].
    current_http: Option<HttpCompletion>,
    /// Entity handles from the most recent `idtx_find_by_type` call, read back
    /// by `idtx_type_query_get` by index. Transient — overwritten each query.
    type_query_entities: Vec<WireEntity>,
    /// USD paths parallel to [`Self::type_query_entities`].
    type_query_paths: Vec<String>,
    /// Transient world borrow, valid only inside a [`WorldScope`].
    world_ptr: *mut World,
    /// Shared widget-interaction coordination (registry + markers + pending
    /// clicks), installed by the session via
    /// [`BehaviorHost::set_widget_interaction`](crate::BehaviorHost) and cloned
    /// into each store (like [`Self::scene_backend`]). `None` in headless hosts
    /// — the widget imports then no-op.
    pub widget: Option<Arc<Mutex<crate::widget::WidgetInteraction>>>,
    /// The widget event currently being delivered to `_idtx_on_event`, seeded by
    /// [`BehaviorHost`](crate::BehaviorHost) before the hook and read back by the
    /// always-linked `idtx_widget_event_*` imports. `None` outside a dispatch.
    current_widget_event: Option<crate::widget::CurrentWidgetEvent>,
}

// SAFETY: the raw pointer is only ever set/cleared on the single host
// thread that owns the `Store`, and dispatch is sequential. wasmtime
// requires `Send` for the store data under the `runtime` feature.
unsafe impl Send for HostState {}

impl std::fmt::Debug for HostState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostState")
            .field("capabilities", &self.capabilities)
            .field("self_entity", &self.self_entity)
            .field("delta_seconds", &self.delta_seconds)
            .field("time_code", &self.time_code)
            .field("world_bound", &(!self.world_ptr.is_null()))
            .finish_non_exhaustive()
    }
}

impl HostState {
    /// Build a fresh host state for a behavior store.
    #[must_use]
    pub fn new(
        capabilities: Capabilities,
        self_entity: WireEntity,
        limits: wasmtime::StoreLimits,
    ) -> Self {
        let work = crate::limits::WorkLimits::default();
        let budget = crate::work::QueueBudget::new(work.queue_items, work.queue_bytes);
        Self::with_work(capabilities, self_entity, limits, budget, work)
    }

    pub(crate) fn with_work(
        capabilities: Capabilities,
        self_entity: WireEntity,
        limits: wasmtime::StoreLimits,
        budget: crate::work::QueueBudget,
        work: crate::limits::WorkLimits,
    ) -> Self {
        Self {
            behavior_context: None,
            spawn_generation: 1,
            hook_error: None,
            spawn_records: std::collections::BTreeMap::new(),
            auth_scopes: idtx_auth::AuthScopes::default(),
            widget_values: std::collections::BTreeMap::new(),
            experience_state: crate::ExperienceState::default(),
            cancel_level_transition: false,
            queue_budget: budget.clone(),
            work_limits: work,
            capabilities,
            self_entity,
            delta_seconds: 0.0,
            time_code: 0.0,
            input: InputSnapshot::default(),
            xr_snapshot: XrSnapshot::default(),
            xr_recenter: glam::Vec3::ZERO,
            camera_pose: CameraPose::default(),
            camera_transform: glam::Mat4::IDENTITY,
            action_ids: HashMap::new(),
            events: EventQueue::with_budget(budget.clone(), work.argument_bytes),
            playback: PlaybackQueue::with_budget(budget.clone(), work.argument_bytes),
            physics_control: PhysicsControlQueue::with_budget(budget.clone(), work.argument_bytes),
            session_edits: SessionEditQueue::with_budget(budget.clone(), work.argument_bytes),
            level_transitions: LevelTransitionQueue::with_budget(
                budget.clone(),
                work.argument_bytes,
            ),
            overlay: OverlayQueue::with_budget(budget.clone(), work.argument_bytes),
            current_event: None,
            limits,
            scene_backend: None,
            network_policy: idtx_net_guard::NetworkPolicy::denied(),
            http_inbox: Arc::new(Mutex::new(HttpInbox::new(budget, work.http_response_bytes))),
            http_outstanding: Arc::new(AtomicUsize::new(0)),
            current_http: None,
            type_query_entities: Vec::new(),
            type_query_paths: Vec::new(),
            world_ptr: std::ptr::null_mut(),
            widget: None,
            current_widget_event: None,
        }
    }

    /// A disabled instance cannot publish more work or retain undelivered work.
    pub(crate) fn disable_work(&mut self) {
        self.cancel_level_transition = false;
        self.spawn_records.clear();
        self.queue_budget.close();
        self.events.clear();
        self.playback.clear();
        self.physics_control.clear();
        self.session_edits.clear();
        self.level_transitions.clear();
        self.overlay.clear();
        if let Ok(mut inbox) = self.http_inbox.lock() {
            inbox.clear();
        }
        if let Some(widget) = &self.widget
            && let Ok(mut widget) = widget.lock()
        {
            widget.prune_expired();
        }
    }

    /// Install a transient world borrow for the current hook. Paired
    /// with [`Self::clear_world`]; used by the host dispatcher, which
    /// cannot hold a [`WorldScope`] borrow across `func.call` (that
    /// would alias the `&mut Store`).
    ///
    /// # Safety
    ///
    /// `world` must outlive the matching [`Self::clear_world`] call and
    /// no other `&mut World` may exist meanwhile (dispatch is
    /// sequential + single-threaded).
    pub fn set_world(&mut self, world: *mut World) {
        self.world_ptr = world;
    }

    /// Clear the transient world borrow.
    pub fn clear_world(&mut self) {
        self.world_ptr = std::ptr::null_mut();
    }

    /// Borrow the world for the current hook. `None` outside a
    /// [`WorldScope`].
    fn world(&self) -> Option<&World> {
        // SAFETY: `world_ptr` is only non-null while a `WorldScope`
        // guard holds a live `&mut World` on this thread; dispatch is
        // sequential so no aliasing `&mut` exists during an import.
        unsafe { self.world_ptr.as_ref() }
    }

    /// Mutably borrow the world for the current hook.
    #[allow(clippy::mut_from_ref)] // pointer guarded by WorldScope
    fn world_mut(&self) -> Option<&mut World> {
        // SAFETY: see `world`. Dispatch is sequential + single-threaded,
        // so the returned `&mut` does not alias any other live borrow.
        unsafe { self.world_ptr.as_mut() }
    }

    // -- xform imports (capability: xform_write / scene_query) --------

    /// Read an entity's local transform.
    pub fn get_xform(&self, entity: WireEntity) -> Result<Xform, ErrorCode> {
        let world = self.world().ok_or(ErrorCode::NotFound)?;
        let e = resolve_live(world, entity)?;
        let t = world.get::<ec::Transform>(e).ok_or(ErrorCode::NotFound)?;
        Ok(Xform::from_trs(t.translation, t.rotation, t.scale))
    }

    /// Write an entity's local transform + mark it dirty.
    pub fn set_xform(&self, entity: WireEntity, x: &Xform) -> Result<(), ErrorCode> {
        if !self.capabilities.xform_write {
            return Err(ErrorCode::CapabilityDenied);
        }
        let world = self.world_mut().ok_or(ErrorCode::NotFound)?;
        let e = resolve_live(world, entity)?;
        {
            let mut t = world
                .get_mut::<ec::Transform>(e)
                .ok_or(ErrorCode::NotFound)?;
            t.translation = x.translation.into();
            t.rotation = x.rotation.into();
            t.scale = x.scale.into();
            t.recompute_matrix();
        }
        // Mark dirty so a downstream transform sweep refreshes the world
        // matrix on the same frame.
        if world.get::<ec::TransformDirty>(e).is_none() {
            world.entity_mut(e).insert(ec::TransformDirty);
        }
        Ok(())
    }

    /// Read an entity's world-space transform.
    ///
    /// Prefers the behavior-bridge [`ec::GlobalTransform`] (present on the
    /// synthetic fallback entities); for hydrated USD prims — which carry no
    /// bridge transform — falls back to the installed [`SceneBackend`], which
    /// returns the prim's stage-space world transform from `idtx-scene`'s
    /// `WorldTransform`.
    pub fn world_xform(&self, entity: WireEntity) -> Result<Xform, ErrorCode> {
        let world = self.world().ok_or(ErrorCode::NotFound)?;
        let e = resolve_live(world, entity)?;
        if let Some(gt) = world.get::<ec::GlobalTransform>(e) {
            let (scale, rotation, translation) = gt.0.to_scale_rotation_translation();
            return Ok(Xform::from_trs(translation, rotation, scale));
        }
        self.scene_backend
            .as_ref()
            .and_then(|b| b.world_xform(world, e))
            .ok_or(ErrorCode::NotFound)
    }

    // -- scene-query imports (capability: scene_query) ----------------

    pub fn curve_length(&self, entity: WireEntity, index: u32) -> Result<f64, ErrorCode> {
        if !self.capabilities.scene_query { return Err(ErrorCode::CapabilityDenied); }
        let world = self.world().ok_or(ErrorCode::NotFound)?;
        let entity = resolve_live(world, entity)?;
        self.scene_backend.as_ref().ok_or(ErrorCode::NotFound)?.curve_length(world, entity, index)
    }

    pub fn curve_samples(&self, entity: WireEntity, index: u32, space: u32, distances: &[f64], out: &mut [idtx_behavior_abi::CurveSample]) -> Result<(), ErrorCode> {
        if !self.capabilities.scene_query { return Err(ErrorCode::CapabilityDenied); }
        if space > 1 || distances.len() > idtx_behavior_abi::MAX_CURVE_QUERY_SAMPLES || distances.len() > out.len() || distances.iter().any(|x| !x.is_finite()) { return Err(ErrorCode::InvalidArgument); }
        let world = self.world().ok_or(ErrorCode::NotFound)?;
        let entity = resolve_live(world, entity)?;
        self.scene_backend.as_ref().ok_or(ErrorCode::NotFound)?.curve_samples(world, entity, index, space, distances, out)
    }

    /// Resolve a prim path to an entity handle.
    ///
    /// Prefers the behavior-bridge [`ec::UsdPath`] scan; for hydrated USD prims
    /// — which carry no bridge path — falls back to the installed
    /// [`SceneBackend`], which scans `idtx-scene`'s `PrimNode.path`.
    pub fn find(&self, path: &str) -> Result<WireEntity, ErrorCode> {
        if !self.capabilities.scene_query {
            return Err(ErrorCode::CapabilityDenied);
        }
        let world = self.world_mut().ok_or(ErrorCode::NotFound)?;
        if let Some(e) = find_by_path(world, path) {
            return Ok(to_wire(e));
        }
        self.scene_backend
            .as_ref()
            .and_then(|b| b.find_by_path(world, path))
            .map(to_wire)
            .ok_or(ErrorCode::NotFound)
    }

    /// An entity's parent.
    ///
    /// Prefers the behavior-bridge [`ec::Parent`]; for hydrated USD prims —
    /// which carry no bridge hierarchy — falls back to the installed
    /// [`SceneBackend`], which derives the parent from `idtx-scene`'s
    /// `PrimNode` path.
    pub fn parent(&self, entity: WireEntity) -> Result<WireEntity, ErrorCode> {
        if !self.capabilities.scene_query {
            return Err(ErrorCode::CapabilityDenied);
        }
        let world = self.world_mut().ok_or(ErrorCode::NotFound)?;
        let e = resolve_live(world, entity)?;
        if let Some(p) = world.get::<ec::Parent>(e) {
            return Ok(to_wire(p.0));
        }
        self.scene_backend
            .as_ref()
            .and_then(|b| b.parent(world, e))
            .map(to_wire)
            .ok_or(ErrorCode::NotFound)
    }

    /// The nearest strict-ancestor collision body of `entity`
    /// (`IdtxCollisionBodyAPI`), or [`WireEntity::NULL`] when it is inside no
    /// body. Unlike [`parent`](Self::parent), "no body" is a normal result
    /// (most colliders have none), so it returns `Ok(NULL)` rather than an
    /// error — the SDK maps `NULL` to `None`.
    pub fn scene_collision_body(&self, entity: WireEntity) -> Result<WireEntity, ErrorCode> {
        if !self.capabilities.scene_query {
            return Err(ErrorCode::CapabilityDenied);
        }
        let world = self.world().ok_or(ErrorCode::NotFound)?;
        let e = resolve_live(world, entity)?;
        Ok(self
            .scene_backend
            .as_ref()
            .and_then(|b| b.collision_body(world, e))
            .map_or(WireEntity::NULL, to_wire))
    }

    /// Read an entity's **effective** visibility — the composed ECS result the
    /// renderer draws from (`idtx-scene`'s `VisibilityComp.visible`), i.e. the
    /// prim's own opinion folded with its ancestor chain, which is the state a
    /// guest `set_visibility` ultimately drives.
    ///
    /// Delegates to the installed [`SceneBackend`]: hydrated USD prims carry the
    /// scene's `VisibilityComp` rather than any behavior-bridge component, and
    /// the host is USD-free. Returns [`ErrorCode::NotFound`] when no backend is
    /// installed (headless) or the prim has no projected visibility.
    pub fn visibility(&self, entity: WireEntity) -> Result<bool, ErrorCode> {
        if !self.capabilities.scene_query {
            return Err(ErrorCode::CapabilityDenied);
        }
        let world = self.world().ok_or(ErrorCode::NotFound)?;
        let e = resolve_live(world, entity)?;
        self.scene_backend
            .as_ref()
            .and_then(|b| b.visibility(world, e))
            .ok_or(ErrorCode::NotFound)
    }

    /// The nearest rigid-body root (`UsdPhysicsRigidBodyAPI`) of `entity` (self or
    /// ancestor), or [`WireEntity::NULL`] when it is inside no rigid body. Gated
    /// on the **physics** capability (it feeds grab suspend/resume). Like
    /// [`scene_collision_body`](Self::scene_collision_body), "no body" is a normal
    /// `Ok(NULL)` result the SDK maps to `None`.
    pub fn scene_physics_body(&self, entity: WireEntity) -> Result<WireEntity, ErrorCode> {
        if !self.capabilities.physics {
            return Err(ErrorCode::CapabilityDenied);
        }
        let world = self.world().ok_or(ErrorCode::NotFound)?;
        let e = resolve_live(world, entity)?;
        Ok(self
            .scene_backend
            .as_ref()
            .and_then(|b| b.physics_body(world, e))
            .map_or(WireEntity::NULL, to_wire))
    }

    /// An entity's `index`-th child.
    pub fn child(&self, entity: WireEntity, index: u32) -> Result<WireEntity, ErrorCode> {
        if !self.capabilities.scene_query {
            return Err(ErrorCode::CapabilityDenied);
        }
        let world = self.world().ok_or(ErrorCode::NotFound)?;
        let e = resolve_live(world, entity)?;
        world
            .get::<ec::Children>(e)
            .and_then(|c| c.0.get(index as usize).copied())
            .map(to_wire)
            .ok_or(ErrorCode::NotFound)
    }

    /// An entity's direct child count.
    #[must_use]
    pub fn child_count(&self, entity: WireEntity) -> u32 {
        let Some(world) = self.world() else {
            return 0;
        };
        let Ok(e) = resolve_live(world, entity) else {
            return 0;
        };
        world
            .get::<ec::Children>(e)
            .map_or(0, |c| u32::try_from(c.0.len()).unwrap_or(u32::MAX))
    }

    // -- scene-interaction imports (capability: scene_query) ----------
    //
    // Delegate to the installed [`SceneBackend`]; `None` backend (headless)
    // yields a miss / `NotFound` rather than trapping.

    /// Cast a ray into the scene. Returns the hit result and, on a hit, the hit
    /// prim's USD path (written to a caller buffer by the import).
    pub fn raycast(
        &self,
        params: &RaycastParams,
    ) -> Result<(RayHitResult, Option<String>), ErrorCode> {
        if !self.capabilities.scene_query {
            return Err(ErrorCode::CapabilityDenied);
        }
        let backend = self.scene_backend.as_ref().ok_or(ErrorCode::NotFound)?;
        let world = self.world_mut().ok_or(ErrorCode::NotFound)?;
        let hit = backend.raycast(world, params);
        let path = (hit.hit != 0)
            .then(|| resolve_live(world, hit.entity).ok())
            .flatten()
            .and_then(|e| backend.path_of(world, e));
        Ok((hit, path))
    }

    /// Cast a ballistic arc into the scene, filling `out_points` with the
    /// sampled polyline. Returns the hit result and, on a hit, the hit prim's
    /// USD path.
    pub fn arc_cast(
        &self,
        params: &ArcCastParams,
        out_points: &mut [glam::Vec3],
    ) -> Result<(ArcHitResult, Option<String>), ErrorCode> {
        if !self.capabilities.scene_query {
            return Err(ErrorCode::CapabilityDenied);
        }
        let backend = self.scene_backend.as_ref().ok_or(ErrorCode::NotFound)?;
        let world = self.world_mut().ok_or(ErrorCode::NotFound)?;
        let hit = backend.arc_cast(world, params, out_points);
        let path = (hit.hit != 0)
            .then(|| resolve_live(world, hit.hit_entity).ok())
            .flatten()
            .and_then(|e| backend.path_of(world, e));
        Ok((hit, path))
    }

    /// Run a prim-type query, caching the matching entities + paths for readout
    /// by [`Self::type_query_get`]. Returns the number of matches.
    pub fn find_by_type(&mut self, type_hash: u64) -> Result<u32, ErrorCode> {
        if !self.capabilities.scene_query {
            return Err(ErrorCode::CapabilityDenied);
        }
        let backend = self.scene_backend.clone().ok_or(ErrorCode::NotFound)?;
        let mut entities: Vec<EcsEntity> = Vec::new();
        let mut paths: Vec<String> = Vec::new();
        {
            let world = self.world_mut().ok_or(ErrorCode::NotFound)?;
            backend.find_by_type(world, type_hash, &mut entities);
            paths.reserve(entities.len());
            for &e in &entities {
                paths.push(backend.path_of(world, e).unwrap_or_default());
            }
        }
        self.type_query_entities = entities.iter().copied().map(to_wire).collect();
        self.type_query_paths = paths;
        Ok(u32::try_from(self.type_query_entities.len()).unwrap_or(u32::MAX))
    }

    /// The cached entity handle + path for the `index`-th result of the most
    /// recent [`Self::find_by_type`], or `None` when out of range.
    #[must_use]
    pub fn type_query_get(&self, index: u32) -> Option<(WireEntity, &str)> {
        let i = index as usize;
        let entity = self.type_query_entities.get(i).copied()?;
        let path = self.type_query_paths.get(i).map_or("", String::as_str);
        Some((entity, path))
    }

    // -- xr imports (capability: xr_hmd) ------------------------------

    /// The current pose for a tracked XR space. Reads the per-frame
    /// snapshot the host seeds before each tick — no live FFI into the XR
    /// platform on the guest hot path.
    #[must_use]
    pub fn xr_pose(&self, space: XrSpace) -> XrPoseResult {
        self.xr_snapshot.get(space)
    }

    /// The current play-space recenter offset (stage-space ground vector).
    /// Read by the `idtx_xr_get_recenter` import; `ZERO` when inactive.
    #[must_use]
    pub fn xr_recenter(&self) -> glam::Vec3 {
        self.xr_recenter
    }

    /// The active view/player camera pose for the current frame. Read by the
    /// `idtx_camera_pose` import; `valid == 0` when none is available.
    #[must_use]
    pub fn camera_pose(&self) -> CameraPose {
        self.camera_pose
    }

    /// The active view/player camera's full world transform (stage space) for
    /// the current frame. Read by the `idtx_camera_transform` import;
    /// `IDENTITY` when no camera pose is available.
    #[must_use]
    pub fn camera_transform(&self) -> glam::Mat4 {
        self.camera_transform
    }

    /// The 26-joint array for `side`, or `None` when the hand is not tracked.
    #[must_use]
    pub fn xr_hand_joints(
        &self,
        side: idtx_behavior_abi::XrSide,
    ) -> Option<&[idtx_behavior_abi::XrHandJointResult; idtx_behavior_abi::XR_HAND_JOINT_COUNT]>
    {
        self.xr_snapshot.hand_joints(side)
    }

    /// Which input source(s) drive `side` this frame (controller / hand / both).
    /// Read by the `idtx_xr_get_tracking_mode` import; [`XrInputSource::NONE`]
    /// when the side is not tracked.
    ///
    /// [`XrInputSource::NONE`]: idtx_behavior_abi::XrInputSource::NONE
    #[must_use]
    pub fn xr_input_source(
        &self,
        side: idtx_behavior_abi::XrSide,
    ) -> idtx_behavior_abi::XrInputSource {
        self.xr_snapshot.input_source(side)
    }

    // -- events (capability: events) ----------------------------------

    /// Post a named event to the queue.
    pub fn post_event(
        &mut self,
        target: WireEntity,
        name: String,
        payload: Vec<u8>,
    ) -> Result<(), ErrorCode> {
        if !self.capabilities.events {
            return Err(ErrorCode::CapabilityDenied);
        }
        self.events.post(target, name, payload)
    }

    // -- playback (capability: playback) ------------------------------

    /// Post a playback-transport command to the queue.
    pub fn post_playback(
        &mut self,
        target: WireEntity,
        clip: String,
        kind: PlaybackCommandKind,
        value: f32,
    ) -> Result<(), ErrorCode> {
        if !self.capabilities.playback {
            return Err(ErrorCode::CapabilityDenied);
        }
        self.playback.post(target, clip, kind, value)
    }

    // -- physics control (capability: physics) ------------------------

    /// Post a deferred physics-control command (grab suspend/resume). The
    /// session drains it after the update hooks and applies it to its physics
    /// subsystem before the physics tick; the host only buffers it (it has no
    /// knowledge of the simulation).
    pub fn post_physics_control(
        &mut self,
        target: WireEntity,
        simulated: bool,
    ) -> Result<(), ErrorCode> {
        if !self.capabilities.physics {
            return Err(ErrorCode::CapabilityDenied);
        }
        self.physics_control.post(target, simulated)
    }

    // -- session-layer authoring (capability: stage_edit) -------------

    /// Post a deferred session-layer edit to the queue. The edit is
    /// applied to the session layer and re-projected by the session after the
    /// update hooks, not on the guest hot path.
    pub fn post_session_edit(
        &mut self,
        target: WireEntity,
        op: SessionEditOp,
    ) -> Result<(), ErrorCode> {
        if !self.capabilities.stage_edit {
            return Err(ErrorCode::CapabilityDenied);
        }
        self.session_edits.post(target, op)
    }

    /// Authored selected-prim data, read only from the session's ECS snapshot.
    pub fn custom_data(&self, entity: WireEntity, key: &str) -> Result<String, ErrorCode> {
        if !self.capabilities.scene_query {
            return Err(ErrorCode::CapabilityDenied);
        }
        if key.is_empty()
            || key.len() > 256
            || key
                .split(':')
                .any(|part| !crate::spawn::valid_identifier(part))
        {
            return Err(ErrorCode::InvalidArgument);
        }
        let world = self.world().ok_or(ErrorCode::NotFound)?;
        let live = resolve_live(world, entity)?;
        let data = world
            .get::<ec::PrimBehaviorData>(live)
            .ok_or(ErrorCode::NotFound)?;
        let json = if let Some(exact) = data.custom_data.get(key) {
            exact.clone()
        } else {
            let mut parts = key.split(':');
            let root = parts.next().ok_or(ErrorCode::InvalidArgument)?;
            let root_json = data.custom_data.get(root).ok_or(ErrorCode::NotFound)?;
            let value: serde_json::Value =
                serde_json::from_str(root_json).map_err(|_| ErrorCode::TypeMismatch)?;
            let mut value = &value;
            for part in parts {
                value = value
                    .as_object()
                    .ok_or(ErrorCode::TypeMismatch)?
                    .get(part)
                    .ok_or(ErrorCode::NotFound)?;
            }
            serde_json::to_string(value).map_err(|_| ErrorCode::TypeMismatch)?
        };
        if json.len() > idtx_behavior_abi::MAX_BEHAVIOR_CONTEXT_BYTES {
            return Err(ErrorCode::InvalidArgument);
        }
        Ok(json)
    }

    /// Exact named-child lookup using the complete host-side USD path.
    pub fn child_named(&self, parent: WireEntity, name: &str) -> Result<WireEntity, ErrorCode> {
        if !self.capabilities.scene_query {
            return Err(ErrorCode::CapabilityDenied);
        }
        if name.len() > 256 || !crate::spawn::valid_identifier(name) {
            return Err(ErrorCode::InvalidArgument);
        }
        let path = {
            let world = self.world().ok_or(ErrorCode::NotFound)?;
            let live = resolve_live(world, parent)?;
            world
                .get::<ec::UsdPath>(live)
                .map(|p| p.0.clone())
                .or_else(|| {
                    self.scene_backend
                        .as_ref()
                        .and_then(|backend| backend.path_of(world, live))
                })
                .ok_or(ErrorCode::NotFound)?
        };
        self.find(&format!("{}/{}", path.trim_end_matches('/'), name))
    }

    /// Post a bounded reference spawn and reserve its status slot until consumption.
    pub fn post_reference_spawn(
        &mut self,
        parent: WireEntity,
        name: String,
        prototype_id: String,
        transform: Xform,
        context_json: String,
        connections: Vec<crate::AttributeConnection>,
    ) -> Result<idtx_behavior_abi::SpawnTicket, ErrorCode> {
        if !self.capabilities.stage_edit {
            return Err(ErrorCode::CapabilityDenied);
        }
        crate::spawn::validate_reference(
            &name,
            &prototype_id,
            &transform,
            &context_json,
            &connections,
        )?;
        let parent = if parent.is_null() {
            self.self_entity
        } else {
            parent
        };
        resolve_live(self.world().ok_or(ErrorCode::NotFound)?, parent)?;
        if self.spawn_records.len() >= MAX_PENDING_SPAWN_TICKETS {
            return Err(ErrorCode::InvalidArgument);
        }
        let reservation = self
            .queue_budget
            .reserve(std::mem::size_of::<SpawnRecord>())?;
        let ticket = NEXT_SPAWN_TICKET
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| ErrorCode::InvalidArgument)?;
        self.session_edits.post(
            parent,
            SessionEditOp::SpawnReference {
                ticket,
                generation: self.spawn_generation,
                name,
                prototype_id,
                transform,
                context_json,
                connections,
            },
        )?;
        self.spawn_records.insert(
            ticket,
            SpawnRecord {
                result: idtx_behavior_abi::SpawnResult::new(
                    idtx_behavior_abi::SpawnStatus::Accepted,
                    WireEntity::NULL,
                    ErrorCode::Ok,
                ),
                _reservation: reservation,
            },
        );
        Ok(idtx_behavior_abi::SpawnTicket(ticket))
    }

    pub(crate) fn spawn_result(
        &self,
        ticket: u64,
    ) -> Result<idtx_behavior_abi::SpawnResult, ErrorCode> {
        self.spawn_records
            .get(&ticket)
            .map(|record| record.result)
            .ok_or(ErrorCode::NotFound)
    }
    pub(crate) fn consume_spawn_result(&mut self, ticket: u64) {
        if self
            .spawn_records
            .get(&ticket)
            .is_some_and(|record| record.result.is_terminal())
        {
            self.spawn_records.remove(&ticket);
        }
    }
    pub(crate) fn set_spawn_result(
        &mut self,
        ticket: u64,
        result: idtx_behavior_abi::SpawnResult,
    ) -> bool {
        let Some(record) = self.spawn_records.get_mut(&ticket) else {
            return false;
        };
        let current = record.result.status();
        let next = result.status();
        if record.result.is_terminal()
            || !matches!(
                (current, next),
                (
                    idtx_behavior_abi::SpawnStatus::Accepted,
                    idtx_behavior_abi::SpawnStatus::Projected
                        | idtx_behavior_abi::SpawnStatus::Failed
                ) | (
                    idtx_behavior_abi::SpawnStatus::Projected,
                    idtx_behavior_abi::SpawnStatus::Ready | idtx_behavior_abi::SpawnStatus::Failed
                )
            )
        {
            return false;
        }
        record.result = result;
        true
    }
    pub(crate) fn clear_spawn_results(&mut self) {
        self.spawn_records.clear();
    }

    // -- level-transition request (capability: level_control) ---------

    /// Post a deferred level-transition request to the queue. The request
    /// is surfaced by the session after the update hooks and performed by the
    /// runtime driver, never on the guest hot path. Defense-in-depth:
    /// returns [`ErrorCode::CapabilityDenied`] when the bucket is false (the
    /// linker gating — the import is absent without the grant — is the primary
    /// guard).
    pub fn post_level_transition(
        &mut self,
        target: String,
        spawn: Option<String>,
    ) -> Result<(), ErrorCode> {
        if !self.capabilities.level_control {
            return Err(ErrorCode::CapabilityDenied);
        }
        self.level_transitions.post(target, spawn)
    }

    // -- overlay / render commands (capability: overlay) --------------

    /// Post a transient overlay / render command to the queue. The
    /// command is drained by the render side after the update hooks and fed
    /// into the overlay pass / post-process chain — it is never authored into
    /// USD and does not survive the frame.
    pub fn post_overlay(&mut self, target: WireEntity, op: OverlayOp) -> Result<(), ErrorCode> {
        if !self.capabilities.overlay {
            return Err(ErrorCode::CapabilityDenied);
        }
        self.overlay.post(target, op)
    }

    /// Seed (or clear) the event currently being delivered to `on_event`.
    /// The dispatcher sets it immediately before the hook fires and clears
    /// it after; the `idtx_event_current_*` imports read it back.
    pub fn set_current_event(&mut self, ev: Option<Event>) {
        self.current_event = ev;
    }

    /// The name of the event currently being delivered, or `None` outside
    /// an `on_event` dispatch.
    #[must_use]
    pub fn event_name(&self) -> Option<&str> {
        self.current_event.as_ref().map(|e| e.name.as_str())
    }

    /// The payload of the event currently being delivered, or `None`
    /// outside an `on_event` dispatch.
    #[must_use]
    pub fn event_payload(&self) -> Option<&[u8]> {
        self.current_event.as_ref().map(|e| e.payload.as_slice())
    }

    /// The target of the event currently being delivered
    /// ([`WireEntity::NULL`] when none / broadcast).
    #[must_use]
    pub fn event_target(&self) -> WireEntity {
        self.current_event
            .as_ref()
            .map_or(WireEntity::NULL, |e| e.target)
    }

    // -- widget interaction (capability: widget) ----------------------

    /// Seed (or clear) the widget event currently being delivered to
    /// `_idtx_on_event`. The host sets it immediately before the hook and clears
    /// it after; the always-linked `idtx_widget_event_*` imports read it back.
    pub fn set_current_widget_event(&mut self, ev: Option<crate::widget::CurrentWidgetEvent>) {
        self.current_widget_event = ev;
    }

    /// `idtx_widget_event_kind`: the current widget event's kind as u32, or
    /// [`idtx_behavior_abi::WIDGET_EVENT_NONE`] when not a widget event.
    #[must_use]
    pub fn widget_event_kind(&self) -> u32 {
        self.current_widget_event
            .as_ref()
            .map_or(idtx_behavior_abi::WIDGET_EVENT_NONE, |e| e.kind.as_u32())
    }

    /// `idtx_widget_event_element`: the current widget event's element path.
    #[must_use]
    pub fn widget_event_element(&self) -> Option<&str> {
        self.current_widget_event
            .as_ref()
            .map(|e| e.element.as_str())
    }

    /// `idtx_widget_event_uv`: the current widget event's panel-local UV.
    #[must_use]
    pub fn widget_event_uv(&self) -> [f32; 2] {
        self.current_widget_event
            .as_ref()
            .map_or([0.0, 0.0], |e| e.uv)
    }

    /// `idtx_widget_register_handler`: record that `panel`'s panel-relative
    /// `element` handles `kind` events (NULL panel resolves to the caller's own
    /// entity). Lets the host report `consumed` synchronously and dispatch only
    /// handled events. Requires the `widget` capability.
    pub fn register_widget_handler(
        &mut self,
        panel: WireEntity,
        element: &str,
        kind: WidgetEventKind,
    ) -> Result<(), ErrorCode> {
        if !self.capabilities.widget {
            return Err(ErrorCode::CapabilityDenied);
        }
        let target = if panel.is_null() {
            self.self_entity
        } else {
            panel
        };
        let widget = self.widget.as_ref().ok_or(ErrorCode::NotFound)?;
        widget
            .lock()
            .map_err(|_| ErrorCode::InvalidArgument)?
            .register_from(target, element, kind, &self.queue_budget)
    }
    /// `idtx_widget_handle`: hand a widget raycast hit (`entity` + stage-space
    /// `position`) to host-side processing. Always records the hit-point cue;
    /// when a `widgets.*` action edge fired on an element with a registered
    /// handler, queues the click for dispatch and returns `true` (consumed).
    /// Requires the `widget` capability.
    pub fn widget_handle(
        &mut self,
        entity: WireEntity,
        position: glam::Vec3,
    ) -> Result<bool, ErrorCode> {
        if !self.capabilities.widget {
            return Err(ErrorCode::CapabilityDenied);
        }
        let backend = self.scene_backend.clone().ok_or(ErrorCode::NotFound)?;
        let info = {
            let world = self.world().ok_or(ErrorCode::NotFound)?;
            let live = resolve_live(world, entity)?;
            backend.widget_hit(world, live, position)
        };
        let Some(info) = info else { return Ok(false) };

        // Always draw the hit-point cue when a panel is under the ray.
        if let Some(w) = self.widget.as_ref() {
            w.lock()
                .map_err(|_| ErrorCode::InvalidArgument)?
                .push_marker_from(
                    crate::widget::WidgetMarker {
                        position: info.position,
                        normal: info.normal,
                    },
                    &self.queue_budget,
                )?;
        }

        // No element under the hit → blocked but nothing to dispatch.
        let Some(element) = info.element else {
            return Ok(false);
        };

        // Which `widgets.*` action fired this frame (just-pressed edge)?
        let Some(kind) = self.fired_widget_kind() else {
            return Ok(false);
        };

        // Dispatch only when a handler is registered (synchronous `consumed`).
        let Some(w) = self.widget.clone() else {
            return Ok(false);
        };
        let Ok(mut lock) = w.lock() else {
            return Ok(false);
        };
        if lock.has_handler(entity, &element, kind) {
            lock.push_pending_from(
                crate::widget::PendingWidgetEvent {
                    panel: entity,
                    element,
                    uv: info.uv,
                    kind,
                },
                &self.queue_budget,
            )?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// The `widgets.*` interaction kind whose action edge fired this frame
    /// (just-pressed), resolved from the behavior's declared action ids. `None`
    /// when no widget action fired. Only `widgets.click` is checked.
    fn fired_widget_kind(&self) -> Option<WidgetEventKind> {
        for kind in [WidgetEventKind::Click] {
            let Some(&id) = self.action_ids.get(kind.action_name()) else {
                continue;
            };
            if self.input.phase(InputActionId(id)) == InputPhase::Pressed {
                return Some(kind);
            }
        }
        None
    }

    // -- async HTTP (capability: net) ---------------------------------

    /// Seed (or clear) the HTTP completion currently being delivered to
    /// `_idtx_on_http_response`. The dispatcher sets it immediately before the
    /// hook fires and clears it after; the `idtx_http_response_*` imports read it
    /// back. Mirrors [`Self::set_current_event`].
    pub fn set_current_http(&mut self, completion: Option<HttpCompletion>) {
        self.current_http = completion;
    }

    pub(crate) fn take_current_http(&mut self) -> Option<HttpCompletion> {
        self.current_http.take()
    }

    /// The status of the HTTP completion currently being delivered, or `0`
    /// outside the callback (and on a transport failure / policy denial).
    #[must_use]
    pub fn http_status(&self) -> u16 {
        self.current_http.as_ref().map_or(0, |c| c.status)
    }

    /// The body of the HTTP completion currently being delivered, or an empty
    /// slice outside the callback.
    #[must_use]
    pub fn http_body(&self) -> &[u8] {
        self.current_http
            .as_ref()
            .map_or(&[], |c| c.body.as_slice())
    }

    /// Drain every pending async-HTTP completion for this instance. Called by
    /// the host at the top of `run_frame`; returns an empty vec when the inbox is
    /// empty (or its lock is poisoned — a poisoned lock means a worker panicked
    /// mid-push, which cannot corrupt the already-pushed completions).
    #[must_use]
    pub fn drain_http_completions(&self) -> Vec<HttpCompletion> {
        self.http_inbox
            .lock()
            .map(|mut q| q.pending.drain())
            .unwrap_or_default()
    }
}

impl Drop for HostState {
    fn drop(&mut self) {
        // Also invalidates in-flight broadcasts, widget ownership, and workers
        // whose reserved completion outlives the store.
        self.queue_budget.close();
    }
}

/// RAII guard installing a `&mut World` borrow into a [`HostState`] for
/// the duration of a hook dispatch, clearing it on drop.
#[derive(Debug)]
pub struct WorldScope<'a> {
    state: &'a mut HostState,
}

impl<'a> WorldScope<'a> {
    /// Install `world` into `state` for the scope's lifetime.
    pub fn new(state: &'a mut HostState, world: &mut World) -> Self {
        state.world_ptr = std::ptr::from_mut(world);
        Self { state }
    }

    /// Borrow the underlying state mutably (e.g. to drain the event
    /// queue after the hook returns but before the scope drops).
    pub fn state(&mut self) -> &mut HostState {
        self.state
    }

    /// Borrow the underlying state immutably. The world-access methods
    /// on [`HostState`] take `&self` (they reach the world through the
    /// guarded raw pointer), so imports + tests can call them through
    /// this shared borrow.
    #[must_use]
    pub fn state_ref(&self) -> &HostState {
        self.state
    }
}

impl Drop for WorldScope<'_> {
    fn drop(&mut self) {
        self.state.world_ptr = std::ptr::null_mut();
    }
}

// ---------------------------------------------------------------------------
// Handle encoding + validation
// ---------------------------------------------------------------------------

/// Convert an ECS entity to the wire handle (`to_bits`).
#[must_use]
pub fn to_wire(e: EcsEntity) -> WireEntity {
    WireEntity::from_bits(e.to_bits())
}

/// Convert a wire handle back to an ECS entity, validating it against
/// the live world (§5.2 stale-handle validation).
///
/// Uses `Entity::try_from_bits` rather than `from_bits`: a hostile or
/// buggy guest can hand us an arbitrary `u64`, and `from_bits` *panics*
/// on structurally-invalid bits — which would abort the whole host. The
/// fallible form maps that to [`ErrorCode::StaleHandle`] instead.
pub fn resolve_live(world: &World, wire: WireEntity) -> Result<EcsEntity, ErrorCode> {
    if wire.is_null() {
        return Err(ErrorCode::NotFound);
    }
    let e = EcsEntity::try_from_bits(wire.to_bits()).ok_or(ErrorCode::StaleHandle)?;
    if world.entities().contains(e) {
        Ok(e)
    } else {
        Err(ErrorCode::StaleHandle)
    }
}

/// Find the entity whose [`ec::UsdPath`] equals `path`.
///
/// Linear scan of the `UsdPath` archetype — acceptable for the MVP; a
/// path→entity index is a follow-up. Takes `&mut World` because
/// `bevy_ecs` query iteration requires it.
fn find_by_path(world: &mut World, path: &str) -> Option<EcsEntity> {
    let mut query = world.query::<(EcsEntity, &ec::UsdPath)>();
    for (e, p) in query.iter(world) {
        if p.0 == path {
            return Some(e);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state() -> HostState {
        HostState::new(
            Capabilities::all(),
            WireEntity::NULL,
            wasmtime::StoreLimits::default(),
        )
    }

    fn spawn_xform() -> Xform {
        Xform {
            translation: glam::Vec3::ZERO.into(),
            rotation: glam::Quat::IDENTITY.into(),
            scale: glam::Vec3::ONE.into(),
            ..Default::default()
        }
    }

    fn spawn(
        state: &mut HostState,
        parent: WireEntity,
    ) -> Result<idtx_behavior_abi::SpawnTicket, ErrorCode> {
        state.post_reference_spawn(
            parent,
            "Panel".into(),
            "overview".into(),
            spawn_xform(),
            "{}".into(),
            vec![],
        )
    }

    #[test]
    fn spawn_ticket_limit_is_independent_of_drained_request_queue() {
        let mut world = World::new();
        let entity = to_wire(world.spawn_empty().id());
        let mut state = make_state();
        let mut scope = WorldScope::new(&mut state, &mut world);
        let mut tickets = Vec::new();
        for _ in 0..MAX_PENDING_SPAWN_TICKETS {
            tickets.push(spawn(scope.state(), entity).unwrap());
            assert_eq!(scope.state().session_edits.drain().len(), 1);
        }
        assert_eq!(
            spawn(scope.state(), entity),
            Err(ErrorCode::InvalidArgument)
        );
        let ticket = tickets[0].0;
        scope.state().consume_spawn_result(ticket);
        assert!(
            scope.state_ref().spawn_result(ticket).is_ok(),
            "nonterminal reads retain the ticket"
        );
        assert!(!scope.state().set_spawn_result(
            ticket,
            idtx_behavior_abi::SpawnResult::new(
                idtx_behavior_abi::SpawnStatus::Ready,
                entity,
                ErrorCode::Ok
            )
        ));
        assert!(scope.state().set_spawn_result(
            ticket,
            idtx_behavior_abi::SpawnResult::new(
                idtx_behavior_abi::SpawnStatus::Failed,
                WireEntity::NULL,
                ErrorCode::NotFound
            )
        ));
        scope.state().consume_spawn_result(ticket);
        assert!(spawn(scope.state(), entity).is_ok());
        scope.state().clear_spawn_results();
        assert!(scope.state_ref().spawn_records.is_empty());
    }

    #[test]
    fn spawn_request_and_ticket_share_queue_credits_and_release_on_failure() {
        let mut world = World::new();
        let entity = to_wire(world.spawn_empty().id());
        let work = crate::limits::WorkLimits::default();
        let budget = crate::work::QueueBudget::new(2, 4096);
        let mut state = HostState::with_work(
            Capabilities::all(),
            entity,
            wasmtime::StoreLimits::default(),
            budget.clone(),
            work,
        );
        let mut scope = WorldScope::new(&mut state, &mut world);
        let ticket = spawn(scope.state(), entity).unwrap().0;
        assert!(
            budget.reserve(0).is_err(),
            "both queued request and result reserve a slot"
        );
        assert_eq!(
            spawn(scope.state(), entity),
            Err(ErrorCode::InvalidArgument)
        );
        assert_eq!(scope.state().session_edits.drain().len(), 1);
        assert_eq!(
            spawn(scope.state(), entity),
            Err(ErrorCode::InvalidArgument),
            "failed queue insertion releases its temporary record reservation"
        );
        assert_eq!(scope.state_ref().spawn_records.len(), 1);
        let remaining = budget.reserve(0).unwrap();
        assert!(budget.reserve(0).is_err());
        drop(remaining);
        assert!(scope.state().set_spawn_result(
            ticket,
            idtx_behavior_abi::SpawnResult::new(
                idtx_behavior_abi::SpawnStatus::Failed,
                WireEntity::NULL,
                ErrorCode::NotFound
            )
        ));
        scope.state().consume_spawn_result(ticket);
        let first = budget.reserve(0).unwrap();
        let second = budget.reserve(0).unwrap();
        drop((first, second));
        assert!(spawn(scope.state(), entity).is_ok());
    }

    #[test]
    fn spawn_rejects_stale_parent_and_denied_capability_without_publishing_work() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        let stale = to_wire(entity);
        world.despawn(entity);
        let replacement = to_wire(world.spawn_empty().id());
        let mut state = make_state();
        let mut scope = WorldScope::new(&mut state, &mut world);
        assert_eq!(spawn(scope.state(), stale), Err(ErrorCode::StaleHandle));
        scope.state().capabilities.stage_edit = false;
        assert_eq!(
            spawn(scope.state(), replacement),
            Err(ErrorCode::CapabilityDenied)
        );
        assert!(scope.state_ref().spawn_records.is_empty());
        assert!(scope.state_ref().session_edits.is_empty());
    }

    #[test]
    fn nested_custom_data_distinguishes_authored_invalid_data_from_missing_keys() {
        for (json, key, expected) in [
            ("null", "idtx:assetOverview", Err(ErrorCode::TypeMismatch)),
            (
                r#""unsupported""#,
                "idtx:assetOverview",
                Err(ErrorCode::TypeMismatch),
            ),
            ("[]", "idtx:assetOverview", Err(ErrorCode::TypeMismatch)),
            (
                r#"{"assetOverview":null}"#,
                "idtx:assetOverview:url",
                Err(ErrorCode::TypeMismatch),
            ),
            (
                r#"{"assetOverview":"invalid"}"#,
                "idtx:assetOverview:url",
                Err(ErrorCode::TypeMismatch),
            ),
            ("{}", "idtx:assetOverview", Err(ErrorCode::NotFound)),
            (
                r#"{"assetOverview":{}}"#,
                "idtx:assetOverview:url",
                Err(ErrorCode::NotFound),
            ),
            (
                r#"{"assetOverview":null}"#,
                "idtx:assetOverview",
                Ok("null".to_owned()),
            ),
            (
                r#"{"assetOverview":{"url":"endpoint"}}"#,
                "idtx:assetOverview:url",
                Ok(r#""endpoint""#.to_owned()),
            ),
        ] {
            let mut world = World::new();
            let entity = to_wire(
                world
                    .spawn(ec::PrimBehaviorData {
                        custom_data: std::collections::BTreeMap::from([(
                            "idtx".into(),
                            json.into(),
                        )]),
                        context: None,
                    })
                    .id(),
            );
            let mut state = make_state();
            let scope = WorldScope::new(&mut state, &mut world);
            assert_eq!(
                scope.state_ref().custom_data(entity, key),
                expected,
                "{json} at {key}"
            );
        }
        let mut world = World::new();
        let entity = to_wire(world.spawn(ec::PrimBehaviorData::default()).id());
        let mut state = make_state();
        let scope = WorldScope::new(&mut state, &mut world);
        assert_eq!(
            scope.state_ref().custom_data(entity, "idtx:assetOverview"),
            Err(ErrorCode::NotFound)
        );
    }

    #[test]
    fn network_policy_defaults_to_denied() {
        // Deny-by-default: even with every capability granted, a fresh host state
        // carries the denied network policy, so a `net` request is rejected unless
        // the session explicitly installs an allowlist (the same SSRF posture as
        // OpenExec REST datasources). The host overwrites this in `attach` only
        // from the policy the session pushed via `set_network_policy`.
        assert!(make_state().network_policy.is_denied());
    }

    #[test]
    fn get_set_xform_round_trips_and_marks_dirty() {
        let mut world = World::new();
        let e = world
            .spawn((
                ec::Transform::default(),
                ec::GlobalTransform::default(),
                ec::UsdPath("/World/Cube".to_owned()),
            ))
            .id();

        let mut state = make_state();
        let scope = WorldScope::new(&mut state, &mut world);
        let wire = to_wire(e);

        let mut x = scope.state_ref().get_xform(wire).unwrap();
        x.translation = idtx_behavior_abi::Vec3 {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        };
        scope.state_ref().set_xform(wire, &x).unwrap();
        drop(scope);

        let t = world.get::<ec::Transform>(e).unwrap();
        assert_eq!(t.translation, glam::Vec3::new(1.0, 2.0, 3.0));
        assert!(world.get::<ec::TransformDirty>(e).is_some());
    }

    #[test]
    fn stale_handle_returns_stale() {
        let mut world = World::new();
        let e = world.spawn(ec::Transform::default()).id();
        world.despawn(e);
        // Spawn a replacement to bump the generation on the recycled slot.
        let _e2 = world.spawn(ec::Transform::default()).id();

        let mut state = make_state();
        let scope = WorldScope::new(&mut state, &mut world);
        let err = scope.state_ref().get_xform(to_wire(e)).unwrap_err();
        assert_eq!(err, ErrorCode::StaleHandle);
    }

    #[test]
    fn find_resolves_path() {
        let mut world = World::new();
        let e = world
            .spawn((
                ec::Transform::default(),
                ec::UsdPath("/World/Camera".to_owned()),
            ))
            .id();

        let mut state = make_state();
        let scope = WorldScope::new(&mut state, &mut world);
        let found = scope.state_ref().find("/World/Camera").unwrap();
        assert_eq!(found, to_wire(e));
        assert_eq!(
            scope.state_ref().find("/nope").unwrap_err(),
            ErrorCode::NotFound
        );
    }

    #[test]
    fn xform_write_denied_without_capability() {
        let mut world = World::new();
        let e = world.spawn(ec::Transform::default()).id();
        let mut state = HostState::new(
            Capabilities::default(),
            WireEntity::NULL,
            wasmtime::StoreLimits::default(),
        );
        let scope = WorldScope::new(&mut state, &mut world);
        let err = scope
            .state_ref()
            .set_xform(to_wire(e), &Xform::default())
            .unwrap_err();
        assert_eq!(err, ErrorCode::CapabilityDenied);
    }

    #[test]
    fn session_edit_denied_without_capability_and_enqueues_when_granted() {
        use crate::session_edit::SessionEditOp;

        // Denied without `stage_edit`.
        let mut denied = HostState::new(
            Capabilities::default(),
            WireEntity::NULL,
            wasmtime::StoreLimits::default(),
        );
        let err = denied
            .post_session_edit(WireEntity::NULL, SessionEditOp::SetVisibility(false))
            .unwrap_err();
        assert_eq!(err, ErrorCode::CapabilityDenied);
        assert!(denied.session_edits.is_empty());

        // Enqueues when granted; no world borrow needed (the edit is deferred).
        let mut granted = make_state(); // Capabilities::all()
        granted
            .post_session_edit(WireEntity::from_bits(3), SessionEditOp::SetVisibility(true))
            .unwrap();
        let drained = granted.session_edits.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].target, WireEntity::from_bits(3));
        assert_eq!(drained[0].op, SessionEditOp::SetVisibility(true));
    }

    #[test]
    fn physics_control_denied_without_capability_and_enqueues_when_granted() {
        // Denied without `physics`.
        let mut denied = HostState::new(
            Capabilities::default(),
            WireEntity::NULL,
            wasmtime::StoreLimits::default(),
        );
        let err = denied
            .post_physics_control(WireEntity::NULL, false)
            .unwrap_err();
        assert_eq!(err, ErrorCode::CapabilityDenied);
        assert!(denied.physics_control.is_empty());

        // Enqueues when granted; no world borrow needed (deferred to the session).
        let mut granted = make_state(); // Capabilities::all()
        granted
            .post_physics_control(WireEntity::from_bits(5), false)
            .unwrap();
        granted
            .post_physics_control(WireEntity::from_bits(5), true)
            .unwrap();
        let drained = granted.physics_control.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].target, WireEntity::from_bits(5));
        assert!(!drained[0].simulated);
        assert!(drained[1].simulated);
    }

    #[test]
    fn overlay_denied_without_capability_and_enqueues_when_granted() {
        use crate::overlay::OverlayOp;
        use glam::Vec4;
        // Denied without `overlay`.
        let mut denied = HostState::new(
            Capabilities::default(),
            WireEntity::NULL,
            wasmtime::StoreLimits::default(),
        );
        let err = denied
            .post_overlay(WireEntity::NULL, OverlayOp::Highlight { color: Vec4::ONE })
            .unwrap_err();
        assert_eq!(err, ErrorCode::CapabilityDenied);
        assert!(denied.overlay.is_empty());

        // Enqueues when granted; no world borrow needed (transient overlay).
        let mut granted = make_state(); // Capabilities::all()
        granted
            .post_overlay(
                WireEntity::from_bits(4),
                OverlayOp::Highlight {
                    color: Vec4::new(1.0, 0.5, 0.0, 1.0),
                },
            )
            .unwrap();
        let drained = granted.overlay.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].target, WireEntity::from_bits(4));
        assert!(matches!(drained[0].op, OverlayOp::Highlight { .. }));
    }

    /// A canned [`SceneBackend`] for the fallback tests: returns fixed world
    /// transform / find / parent results, so we can assert the host falls back
    /// to it when the behavior-bridge components are absent (hydrated prims).
    #[derive(Debug)]
    struct MockBackend {
        world_xform: Xform,
        find_entity: Option<EcsEntity>,
        parent_entity: Option<EcsEntity>,
        visible: Option<bool>,
    }

    impl SceneBackend for MockBackend {
        fn raycast(&self, _: &mut World, _: &RaycastParams) -> RayHitResult {
            RayHitResult::default()
        }
        fn arc_cast(&self, _: &mut World, _: &ArcCastParams, _: &mut [glam::Vec3]) -> ArcHitResult {
            ArcHitResult::default()
        }
        fn find_by_type(&self, _: &mut World, _: u64, out: &mut Vec<EcsEntity>) {
            out.clear();
        }
        fn path_of(&self, _: &World, _: EcsEntity) -> Option<String> {
            None
        }
        fn world_xform(&self, _: &World, _: EcsEntity) -> Option<Xform> {
            Some(self.world_xform)
        }
        fn find_by_path(&self, _: &mut World, _: &str) -> Option<EcsEntity> {
            self.find_entity
        }
        fn parent(&self, _: &mut World, _: EcsEntity) -> Option<EcsEntity> {
            self.parent_entity
        }
        fn visibility(&self, _: &World, _: EcsEntity) -> Option<bool> {
            self.visible
        }
    }

    fn mock(backend: MockBackend) -> Arc<dyn SceneBackend> {
        Arc::new(backend)
    }

    #[test]
    fn world_xform_falls_back_to_scene_backend_for_hydrated_prims() {
        let mut world = World::new();
        // A hydrated-prim-like entity: no bridge `GlobalTransform`.
        let e = world
            .spawn(ec::UsdPath("/World/Player/Pointers/Right".to_owned()))
            .id();
        let canned = Xform::from_trs(
            glam::Vec3::new(5.0, 0.0, 2.0),
            glam::Quat::IDENTITY,
            glam::Vec3::ONE,
        );

        let mut state = make_state();
        state.scene_backend = Some(mock(MockBackend {
            world_xform: canned,
            find_entity: None,
            parent_entity: None,
            visible: None,
        }));
        let scope = WorldScope::new(&mut state, &mut world);
        let got = scope.state_ref().world_xform(to_wire(e)).unwrap();
        assert_eq!(got.translation, canned.translation);
    }

    #[test]
    fn world_xform_prefers_bridge_component_over_backend() {
        let mut world = World::new();
        let bridge = glam::Mat4::from_translation(glam::Vec3::new(1.0, 2.0, 3.0));
        let e = world
            .spawn((ec::Transform::default(), ec::GlobalTransform(bridge)))
            .id();

        let mut state = make_state();
        // Backend returns a different value; the bridge component must win.
        state.scene_backend = Some(mock(MockBackend {
            world_xform: Xform::from_trs(
                glam::Vec3::new(9.0, 9.0, 9.0),
                glam::Quat::IDENTITY,
                glam::Vec3::ONE,
            ),
            find_entity: None,
            parent_entity: None,
            visible: None,
        }));
        let scope = WorldScope::new(&mut state, &mut world);
        let got = scope.state_ref().world_xform(to_wire(e)).unwrap();
        assert_eq!(
            got.translation,
            idtx_behavior_abi::Vec3 {
                x: 1.0,
                y: 2.0,
                z: 3.0
            }
        );
    }

    #[test]
    fn find_falls_back_to_scene_backend() {
        let mut world = World::new();
        // No bridge `UsdPath` on this entity, so the primary scan misses.
        let hydrated = world.spawn(()).id();
        let mut state = make_state();
        state.scene_backend = Some(mock(MockBackend {
            world_xform: Xform::default(),
            find_entity: Some(hydrated),
            parent_entity: None,
            visible: None,
        }));
        let scope = WorldScope::new(&mut state, &mut world);
        assert_eq!(
            scope.state_ref().find("/World/Player").unwrap(),
            to_wire(hydrated)
        );
    }

    #[test]
    fn parent_falls_back_to_scene_backend() {
        let mut world = World::new();
        let parent = world.spawn(()).id();
        let child = world.spawn(()).id(); // no bridge `Parent`
        let mut state = make_state();
        state.scene_backend = Some(mock(MockBackend {
            world_xform: Xform::default(),
            find_entity: None,
            parent_entity: Some(parent),
            visible: None,
        }));
        let scope = WorldScope::new(&mut state, &mut world);
        assert_eq!(
            scope.state_ref().parent(to_wire(child)).unwrap(),
            to_wire(parent)
        );
    }

    #[test]
    fn visibility_reads_from_scene_backend() {
        let mut world = World::new();
        let e = world.spawn(()).id();
        let mut state = make_state();
        state.scene_backend = Some(mock(MockBackend {
            world_xform: Xform::default(),
            find_entity: None,
            parent_entity: None,
            visible: Some(false),
        }));
        let scope = WorldScope::new(&mut state, &mut world);
        assert!(!scope.state_ref().visibility(to_wire(e)).unwrap());
    }

    #[test]
    fn visibility_not_found_without_backend() {
        let mut world = World::new();
        let e = world.spawn(()).id();
        let mut state = make_state(); // no scene backend installed
        let scope = WorldScope::new(&mut state, &mut world);
        assert_eq!(
            scope.state_ref().visibility(to_wire(e)).unwrap_err(),
            ErrorCode::NotFound
        );
    }

    #[test]
    fn visibility_denied_without_scene_query() {
        let mut world = World::new();
        let e = world.spawn(()).id();
        let mut state = HostState::new(
            Capabilities::default(), // no scene_query
            WireEntity::NULL,
            wasmtime::StoreLimits::default(),
        );
        state.scene_backend = Some(mock(MockBackend {
            world_xform: Xform::default(),
            find_entity: None,
            parent_entity: None,
            visible: Some(true),
        }));
        let scope = WorldScope::new(&mut state, &mut world);
        assert_eq!(
            scope.state_ref().visibility(to_wire(e)).unwrap_err(),
            ErrorCode::CapabilityDenied
        );
    }
}
