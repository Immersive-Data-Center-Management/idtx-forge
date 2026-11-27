//! [`BehaviorHost`] — the top-level host object (§5).
//!
//! Owns the wasmtime [`Engine`], a module registry keyed by uuid, and a
//! per-(entity, uuid) instance table. Drives lifecycle hooks in the
//! order §5.4 prescribes and feeds them the live ECS `World` for the
//! duration of each call via [`WorldScope`].

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ops::Bound::{Excluded, Included};

use bevy_ecs::world::World;
use idtx_behavior_abi::{Entity as WireEntity, InputActionId, InputPhase, StageEvent};
use wasmtime::{Engine, Instance, Store, TypedFunc, WasmParams};

use crate::input::{InputRouter, InputSnapshot};
use crate::limits::{EpochTicker, Limits, spawn_epoch_ticker};
use crate::manifest::BehaviorManifest;
use crate::module::{CompiledModule, ModuleError};
use crate::stage_access::{HostState, to_wire};
use crate::work::{DispatchBudget, QueueBudget, Queued};
use crate::xr_snapshot::XrSnapshot;
use std::sync::{Arc, Mutex};

/// Whether the stage is playing (runtime hooks fire) or authoring
/// (editor hooks fire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    /// Authoring mode — only `on_editor_update` (+ attach/detach) fire.
    Authoring,
    /// Playing mode — the full runtime lifecycle fires.
    Playing,
}

/// How many `Trap::Interrupt` (budget-overrun) traps an instance may
/// accumulate before it is permanently disabled.
///
/// A single interrupt can be a benign scheduling hiccup (the OS
/// descheduled the guest thread past its wall-clock budget), so we
/// tolerate a few and only disable an instance that *repeatedly* blows
/// its budget — that indicates a genuinely runaway hook rather than a
/// one-off stall. Non-interrupt traps (a real guest fault) still disable
/// on the first occurrence.
const MAX_INTERRUPT_TRAPS: u32 = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    PendingAttach,
    Attached,
    Started,
}

/// A live behavior instance bound to one entity.
struct LiveInstance {
    initial_stage_events: VecDeque<StageEvent>,
    stage_initialized: bool,
    id: u64,
    uuid: String,
    store: Store<HostState>,
    instance: Instance,
    /// `true` for a stage-scoped instance (bound to a `/StageBehavior` scope
    /// prim but running stage-wide): it receives [`BehaviorHost::
    /// dispatch_stage_event`] broadcasts and its `self_entity` is
    /// [`WireEntity::NULL`]. Entity-scoped instances are `false`. This replaces
    /// the old `key.entity.is_null()` test, which collapsed every stage
    /// behavior onto the single `NULL` key — two stage behaviors sharing a
    /// module uuid would overwrite each other.
    stage_scoped: bool,
    /// Attach/start may be deferred when a preceding hook used the batch budget.
    lifecycle: Lifecycle,
    /// Cooperative overrun: no more hooks until the next frame.
    yielded: bool,
    /// Diagnostics: last / peak hook time.
    last_hook_us: u128,
    peak_hook_us: u128,
    /// Count of *fatal* traps — a real guest fault, or too many
    /// interrupts. The instance is disabled once this is `> 0`.
    traps: u32,
    startup_error: Option<idtx_behavior_abi::ErrorCode>,
    /// Count of tolerated `Trap::Interrupt` (budget-overrun) traps seen
    /// so far. Promotes to a fatal trap once it exceeds
    /// [`MAX_INTERRUPT_TRAPS`].
    interrupt_traps: u32,
}

enum Delivery {
    Event(Queued<crate::Event>),
    Widget(Queued<crate::PendingWidgetEvent>),
}

impl Delivery {
    fn cancelled(&self) -> bool {
        match self {
            Self::Event(e) => e.cancelled(),
            Self::Widget(e) => e.cancelled(),
        }
    }

    fn target(&self) -> WireEntity {
        match self {
            Self::Event(e) => e.value.target,
            Self::Widget(e) => e.value.panel,
        }
    }
}

/// Cursor over stable instance IDs, not a cloned recipient vector per event.
/// New/replaced instances cannot inherit an old broadcast midway through it.
struct PendingDelivery {
    delivery: Delivery,
    after: u64,
    through: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HookCall {
    Missing,
    Deferred,
    Invoked,
    Failed,
    Disabled,
}

/// Identifies a behavior instance: the entity it's attached to + the
/// module uuid.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InstanceKey {
    /// The binding entity — the prim the behavior is authored on. For an
    /// entity-scoped behavior this is its own prim's entity; for a
    /// stage-scoped behavior this is its `/StageBehavior` scope prim's entity
    /// (distinct per level), **not** `NULL`. Keying by the real prim entity is
    /// what keeps two stage behaviors that share a module uuid from colliding.
    /// The stage-wide `NULL` identity lives in [`HostState::self_entity`]
    /// instead (see [`BehaviorHost::attach_stage`]).
    pub entity: WireEntity,
    /// The module uuid.
    pub uuid: String,
}

/// Top-level WASM behavior host.
pub struct BehaviorHost {
    spawn_generation: u64,
    experience_state: crate::ExperienceState,
    engine: Engine,
    /// Compiled modules keyed by uuid.
    modules: HashMap<String, CompiledModule>,
    /// Live instances keyed by (entity, uuid).
    instances: HashMap<InstanceKey, LiveInstance>,
    instance_order: BTreeMap<u64, InstanceKey>,
    next_instance_id: u64,
    next_playback_sequence: u64,
    frame_round: usize,
    capture_round: usize,
    capture_exhausted: bool,
    update_round: usize,
    http_round: usize,
    spawn_round: usize,
    frame_exhausted: bool,
    pending_deliveries: VecDeque<PendingDelivery>,
    work_budget: QueueBudget,
    #[cfg(feature = "metrics")]
    dispatch_metrics: crate::DispatchMetrics,
    /// Input router producing per-frame snapshots.
    router: InputRouter,
    /// Current resolved input snapshot (updated at step 1 of the loop).
    input: InputSnapshot,
    /// This frame's button edge events (id, phase) derived by
    /// [`Self::update_input`] and delivered to `_idtx_on_input` during
    /// [`Self::run_frame`]. Drained (taken) each frame.
    pending_input_events: Vec<(InputActionId, InputPhase)>,
    /// Current XR pose snapshot (updated before each frame's dispatch).
    xr_snapshot: XrSnapshot,
    /// Current play-space recenter offset, seeded into each store before every
    /// frame beside [`Self::xr_snapshot`] and read by the `idtx_xr_get_recenter`
    /// import. `ZERO` when no recenter is active.
    xr_recenter: glam::Vec3,
    /// Current active view/player camera pose, seeded into each store before
    /// every frame and read by the `idtx_camera_pose` import. Lets a behavior
    /// raycast from the desktop/player camera (or the HMD in XR).
    camera_pose: idtx_behavior_abi::CameraPose,
    /// Current active view/player camera world transform (stage space), seeded
    /// into each store beside [`Self::camera_pose`] and read by the
    /// `idtx_camera_transform` import. Lets a behavior rigidly attach a grabbed
    /// prim to the camera frame. `IDENTITY` when no camera this frame.
    camera_transform: glam::Mat4,
    input_capture: Arc<Mutex<crate::input_capture::InputCapture>>,
    /// Current time-code.
    time_code: f64,
    /// Scene-query backend (raycast / arc-cast / prim-type query), installed by
    /// the session via [`Self::set_scene_backend`] and cloned into each store's
    /// [`HostState`] at [`Self::attach`]. `None` for headless hosts.
    scene_backend: Option<std::sync::Arc<dyn crate::SceneBackend>>,
    /// Shared widget-interaction state (ABI 3): handler registry + per-frame
    /// hit-point cues + pending clicks. Owned here, cloned into each store's
    /// [`HostState`] at [`Self::attach`]. The `handle`/`register` imports mutate
    /// it; [`Self::run_frame`] drains pending clicks (Step 7) and the session
    /// drains markers via [`Self::drain_widget_markers`].
    widget: std::sync::Arc<std::sync::Mutex<crate::widget::WidgetInteraction>>,
    /// Network-egress policy for the `net` capability, installed by the session
    /// via [`Self::set_network_policy`] and cloned into each store's
    /// [`HostState`] at [`Self::attach`]. Defaults to
    /// [`idtx_net_guard::NetworkPolicy::denied`] so behavior HTTP is denied unless
    /// a runtime configures a `[network]` allowlist — the same deny-by-default
    /// gate `OpenExec` REST datasources use.
    network_policy: idtx_net_guard::NetworkPolicy,
    auth_scopes: idtx_auth::AuthScopes,
    /// Execution limits.
    limits: Limits,
    /// Background epoch ticker (kept alive for the host's lifetime).
    _ticker: EpochTicker,
}

impl std::fmt::Debug for BehaviorHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BehaviorHost")
            .field("modules", &self.modules.len())
            .field("instances", &self.instances.len())
            .finish_non_exhaustive()
    }
}

impl BehaviorHost {
    /// Build a host with the given execution limits + input router.
    ///
    /// # Panics
    ///
    /// Panics only if wasmtime rejects the fixed engine configuration
    /// (epoch interruption + fuel metering) — which cannot happen for the
    /// features enabled here.
    #[must_use]
    pub fn new(limits: Limits, router: InputRouter) -> Self {
        let mut config = wasmtime::Config::new();
        // Fine-grained epoch interruption (wall-clock guard) + fuel
        // metering (deterministic op-count guard). Both are armed for startup
        // by `CompiledModule::instantiate`, then by the aggregate dispatcher.
        config.epoch_interruption(true);
        config.consume_fuel(true);
        // On visionOS (W^X: no JIT'd executable pages without special
        // entitlements) switch wasmtime to the Pulley bytecode interpreter.
        // Cranelift still compiles the guest `.wasm`, but to portable Pulley
        // bytecode rather than native machine code, so the engine allocates no
        // executable pages. `epoch_interruption` + `consume_fuel` are runtime
        // features that remain in force under the interpreter, so the per-hook
        // budget/trap machinery is unchanged. Gated on the `visionos` cargo
        // feature (not `target_os`) so the host test suite can validate the
        // interpreter path on any platform with `--features visionos`.
        #[cfg(feature = "visionos")]
        {
            config
                .target("pulley64")
                .expect("pulley64 is a wasmtime-supported interpreter target");
            // The Pulley interpreter bounds-checks linear-memory accesses in
            // software, so wasmtime's default ~4 GiB per-memory virtual
            // reservation + 32 MiB guard region (which exist only to let native
            // code elide bounds checks via guard-page faults) buy nothing here —
            // and reserving that per attached behavior exhausts the visionOS
            // process address space: each instance mmap's 0x1_0000_0000 bytes and
            // the 4th attach fails with "mmap failed to reserve 0x100000000
            // bytes". Drop both to zero so each memory reserves only what the
            // module actually needs (growing dynamically), letting many behaviors
            // attach. No effect on the native (Cranelift) desktop/Quest path,
            // which is not built with this feature and keeps the fast reservation.
            config.memory_reservation(0);
            config.memory_guard_size(0);
            // ...but `memory_reservation(0)` alone is not enough: with it, a
            // dynamic memory is allocated as `module_initial + reservation_for
            // _growth`, and `memory_reservation_for_growth` STILL defaults to
            // 2 GiB on 64-bit. So each behavior instead mmap'd ~2 GiB
            // (`0x80110000` = 17 initial pages + 0x8000_0000), and the ~7th
            // attach exhausted the visionOS address space — `xr_player_move` on
            // `/World/Player` then failed to attach, its play-space recenter
            // bake never ran, and the eye still subtracted the recenter, so the
            // world drifted with the head. The memory can never exceed the
            // per-instance `memory_cap` (enforced via `StoreLimits`), so the
            // growth reservation never needs to be larger than that cap; sizing
            // it to the cap keeps the virtual footprint bounded (cap × instances)
            // while avoiding a relocate on every small grow.
            config.memory_reservation_for_growth(limits.memory_cap as u64);
        }
        // `new` with a valid config cannot fail for these features.
        let engine = Engine::new(&config).expect("valid wasmtime config");
        // The ticker advances the epoch at a fine, fixed granularity so
        // the per-hook deadline is phase-independent (see `limits`).
        let ticker = spawn_epoch_ticker(engine.clone());
        let work_budget =
            QueueBudget::new(limits.work.host_queue_items, limits.work.host_queue_bytes);
        Self {
            engine,
            spawn_generation: 1,
            experience_state: crate::ExperienceState::default(),
            modules: HashMap::new(),
            instances: HashMap::new(),
            instance_order: BTreeMap::new(),
            next_instance_id: 0,
            next_playback_sequence: 1,
            frame_round: 0,
            capture_round: 0,
            capture_exhausted: false,
            update_round: 0,
            http_round: 0,
            spawn_round: 0,
            frame_exhausted: false,
            pending_deliveries: VecDeque::new(),
            work_budget: work_budget.clone(),
            #[cfg(feature = "metrics")]
            dispatch_metrics: crate::DispatchMetrics::default(),
            router,
            input: InputSnapshot::default(),
            pending_input_events: Vec::new(),
            xr_snapshot: XrSnapshot::default(),
            time_code: 0.0,
            xr_recenter: glam::Vec3::ZERO,
            camera_pose: idtx_behavior_abi::CameraPose::default(),
            camera_transform: glam::Mat4::IDENTITY,
            input_capture: Arc::new(Mutex::new(crate::input_capture::InputCapture::default())),
            scene_backend: None,
            widget: std::sync::Arc::new(std::sync::Mutex::new(
                crate::widget::WidgetInteraction::with_budget(work_budget, limits.work),
            )),
            network_policy: idtx_net_guard::NetworkPolicy::denied(),
            auth_scopes: idtx_auth::AuthScopes::default(),
            limits,
            _ticker: ticker,
        }
    }

    /// Build a host with default (conductor) limits + an empty router.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(Limits::conductor(), InputRouter::default())
    }

    #[must_use]
    pub fn experience_state(&self) -> crate::ExperienceState {
        self.experience_state.clone()
    }

    pub fn set_experience_state(&mut self, state: crate::ExperienceState) {
        for live in self.instances.values_mut() {
            live.store.data_mut().experience_state = state.clone();
        }
        self.experience_state = state;
    }

    pub fn drain_level_cancellation(&mut self) -> bool {
        let mut cancelled = false;
        for live in self.instances.values_mut() {
            cancelled |= std::mem::take(&mut live.store.data_mut().cancel_level_transition);
        }
        cancelled
    }

    /// Replace the input router (e.g. after loading `input_map.toml` or a
    /// mapping-context reload). Clears the last snapshot + pending edge events
    /// so no stale input diff leaks across the swap.
    pub fn set_router(&mut self, router: InputRouter) {
        self.router = router;
        if let Ok(mut focus) = self.input_capture.lock() { focus.clear(); }
        for live in self.instances.values_mut() {
            for (name, id) in &mut live.store.data_mut().action_ids { *id = self.router.resolve(name).0; }
        }
        self.input = InputSnapshot::default();
        self.pending_input_events.clear();
    }

    /// Borrow the input router (tests / mapping-context diagnostics).
    #[must_use]
    pub fn router(&self) -> &InputRouter {
        &self.router
    }

    /// Install the scene-query backend used by the raycast / arc-cast /
    /// prim-type-query imports. Cloned into every store's [`HostState`] at
    /// [`Self::attach`], so call this before attaching behaviors (the session
    /// wires it at host construction). Without it those imports return
    /// [`idtx_behavior_abi::ErrorCode::NotFound`] / a miss.
    pub fn set_scene_backend(&mut self, backend: std::sync::Arc<dyn crate::SceneBackend>) {
        self.scene_backend = Some(backend);
    }

    /// Drain this frame's accumulated widget hit-point cues (ABI 3). The session
    /// calls this after [`Self::run_frame`] and renders each as an overlay marker
    /// — so the cue is host-drawn, independent of any behavior's `overlay`
    /// capability. Leaves the buffer empty.
    #[must_use]
    pub fn drain_widget_markers(&self) -> Vec<crate::widget::WidgetMarker> {
        self.widget
            .lock()
            .map(|mut w| w.drain_markers())
            .unwrap_or_default()
    }

    pub fn widget_highlights(&self) -> Vec<(WireEntity, String)> {
        self.widget.lock().map(|w| w.highlights()).unwrap_or_default()
    }

    pub fn input_focus_owners(&self) -> Vec<crate::input_capture::FocusOwner> {
        self.input_capture.lock().map(|f| f.diagnostics()).unwrap_or_default()
    }

    /// Feed a platform UI activation into the same bounded widget dispatcher
    /// used by pointer/controller/hand hits. Only registered elements accept it.
    pub fn post_widget_click(&self, panel: WireEntity, element: &str) -> bool {
        let Ok(mut widget) = self.widget.lock() else {
            return false;
        };
        if !widget.has_handler(panel, element, idtx_behavior_abi::WidgetEventKind::Click) {
            return false;
        }
        widget
            .push_pending(crate::PendingWidgetEvent {
                source: idtx_behavior_abi::PointerSource::Unspecified,
                panel,
                element: element.into(),
                uv: [0.0; 2],
                kind: idtx_behavior_abi::WidgetEventKind::Click,
            })
            .is_ok()
    }

    /// Install the network-egress policy enforced by the `net` capability's
    /// `idtx_http_request` import. Cloned into every store's [`HostState`] at
    /// [`Self::attach`], so call this before attaching behaviors. Defaults to
    /// [`idtx_net_guard::NetworkPolicy::denied`] (deny-by-default), so without a
    /// configured allowlist every behavior HTTP request is rejected — the same
    /// gate `OpenExec` REST datasources use. The session wires it from its
    /// `[network]` config (see `StudioSession::set_exec_network_policy`).
    pub fn set_network_policy(&mut self, policy: idtx_net_guard::NetworkPolicy) {
        self.network_policy = policy;
    }

    /// Credential destinations authorized for guest `auth_write`, independent
    /// of which hosts the network policy permits the guest to contact.
    pub fn set_auth_scopes(&mut self, scopes: idtx_auth::AuthScopes) {
        for live in self.instances.values_mut() {
            live.store.data_mut().auth_scopes = scopes.clone();
        }
        self.auth_scopes = scopes;
    }

    /// Borrow the engine (tests / advanced embedders).
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Compile + register a module by uuid. Returns the uuid.
    pub fn register_module(
        &mut self,
        wasm_bytes: &[u8],
        manifest: BehaviorManifest,
    ) -> Result<String, ModuleError> {
        let uuid = manifest.uuid.clone();
        let compiled = CompiledModule::compile(&self.engine, wasm_bytes, manifest)?;
        self.modules.insert(uuid.clone(), compiled);
        Ok(uuid)
    }

    /// `true` when a module with `uuid` is registered.
    #[must_use]
    pub fn has_module(&self, uuid: &str) -> bool {
        self.modules.contains_key(uuid)
    }

    /// Attach an entity-scoped behavior instance to `entity`, firing
    /// `on_attach`.
    ///
    /// The instance is keyed by `(entity, uuid)` and its `self_entity` is
    /// `entity`, so a self-targeted (`NULL`) edit resolves back to `entity`.
    /// The resolved action-id table is seeded from the router so
    /// `idtx_input_resolve` returns valid ids.
    pub fn attach(
        &mut self,
        world: &mut World,
        entity: WireEntity,
        uuid: &str,
    ) -> Result<(), ModuleError> {
        self.attach_inner(world, entity, entity, false, uuid, None)
    }

    /// Attach a *stage-scoped* behavior bound to `source` — the behavior's
    /// `/StageBehavior` scope prim entity (distinct per level) — but running
    /// stage-wide.
    ///
    /// The instance is keyed by `(source, uuid)` so two stage behaviors
    /// sharing a module uuid across composed levels do not collide onto a
    /// single `NULL` key. Its `self_entity` is still [`WireEntity::NULL`]
    /// (a stage behavior has no concrete "self" prim), so a self-targeted
    /// (`NULL`) edit stays stage-wide and the guest's `idtx_self_entity`
    /// import reads `NULL`. The instance is marked stage-scoped so
    /// [`Self::dispatch_stage_event`] reaches it.
    pub fn attach_stage(
        &mut self,
        world: &mut World,
        source: WireEntity,
        uuid: &str,
    ) -> Result<(), ModuleError> {
        self.attach_inner(world, source, WireEntity::NULL, true, uuid, None)
    }

    /// Attach with immutable context seeded before instantiation and on_attach.
    pub fn attach_with_context(
        &mut self,
        world: &mut World,
        entity: WireEntity,
        uuid: &str,
        context: Option<&str>,
    ) -> Result<(), ModuleError> {
        self.attach_inner(world, entity, entity, false, uuid, context)
    }

    /// Stage-scoped equivalent of attach_with_context.
    pub fn attach_stage_with_context(
        &mut self,
        world: &mut World,
        source: WireEntity,
        uuid: &str,
        context: Option<&str>,
    ) -> Result<(), ModuleError> {
        self.attach_inner(world, source, WireEntity::NULL, true, uuid, context)
    }

    /// Update a ticket only in the store that owns it. False means it was cancelled.
    pub fn finish_stage_edit(&mut self, ticket: u64, error: idtx_behavior_abi::ErrorCode) -> bool {
        self.instances.values_mut().any(|live| live.store.data_mut().finish_edit(ticket, error))
    }

    #[must_use]
    pub fn has_pending_stage_edit(&self, ticket: u64) -> bool {
        self.instances.values().any(|live| live.store.data().edit_result(ticket).is_ok_and(|r| !r.is_terminal()))
    }

    /// Full stop/replacement retires old result slots as well as queued work.
    pub fn clear_stage_edit_results(&mut self) {
        for live in self.instances.values_mut() {
            live.store.data_mut().clear_stage_edit_results();
        }
    }

    /// Update a ticket only in the store that owns it. False means it was cancelled.
    pub fn set_spawn_result(
        &mut self,
        ticket: u64,
        result: idtx_behavior_abi::SpawnResult,
    ) -> bool {
        self.instances
            .values_mut()
            .any(|live| live.store.data_mut().set_spawn_result(ticket, result))
    }

    /// Whether a requester still retains the ticket (not despawned/stopped/consumed).
    #[must_use]
    pub fn has_spawn_ticket(&self, ticket: u64) -> bool {
        self.instances
            .values()
            .any(|live| live.store.data().spawn_result(ticket).is_ok())
    }

    /// Tag future requests with the current session/play generation. When the
    /// generation actually changes — which only happens on a level/stage teardown
    /// (both teardown paths bump it before calling this) — also drop each store's
    /// pending callback-mode spawn completions, so stale completions never survive
    /// to a later frame. A no-op re-set of the same generation (e.g. rebuilding the
    /// scene backend on a basis change) leaves pending completions intact.
    pub fn set_spawn_generation(&mut self, generation: u64) {
        let changed = generation != self.spawn_generation;
        self.spawn_generation = generation;
        for live in self.instances.values_mut() {
            let data = live.store.data_mut();
            data.spawn_generation = generation;
            if changed {
                data.clear_spawn_completions();
                data.cancel_pending_edits();
            }
        }
    }

    /// Clear outstanding tickets when the session stops or changes generation.
    pub fn clear_spawn_results(&mut self) {
        for live in self.instances.values_mut() {
            live.store.data_mut().clear_spawn_results();
        }
    }

    /// Drop every store's pending callback-mode spawn completions, leaving the
    /// spawn records intact. The session teardown paths fail outstanding tickets
    /// (which re-enqueues completions) *after* bumping the generation; calling
    /// this at the end of teardown guarantees no pre-teardown completion survives
    /// to a later frame's `deliver_spawns` phase.
    pub fn clear_spawn_completions(&mut self) {
        for live in self.instances.values_mut() {
            live.store.data_mut().clear_spawn_completions();
        }
    }

    /// Read readiness separately from entity projection. Startup failures never become ready.
    #[must_use]
    pub fn instance_readiness(
        &self,
        entity: WireEntity,
        uuid: &str,
        playing: bool,
    ) -> Option<idtx_behavior_abi::SpawnStatus> {
        use idtx_behavior_abi::SpawnStatus;
        let live = self.instances.get(&InstanceKey {
            entity,
            uuid: uuid.into(),
        })?;
        Some(if live.traps > 0 || live.startup_error.is_some() {
            SpawnStatus::Failed
        } else if live.lifecycle == Lifecycle::Started
            || (!playing && live.lifecycle == Lifecycle::Attached)
        {
            SpawnStatus::Ready
        } else if live.lifecycle == Lifecycle::Attached {
            SpawnStatus::Projected
        } else {
            SpawnStatus::Accepted
        })
    }

    #[must_use]
    pub fn instance_startup_error(
        &self,
        entity: WireEntity,
        uuid: &str,
    ) -> Option<idtx_behavior_abi::ErrorCode> {
        self.instances
            .get(&InstanceKey {
                entity,
                uuid: uuid.into(),
            })
            .and_then(|live| {
                live.startup_error
                    .or((live.traps > 0).then_some(idtx_behavior_abi::ErrorCode::InvalidArgument))
            })
    }

    /// Attach a behavior instance, firing `on_attach`.
    ///
    /// `key_entity` is the binding identity (the behavior's prim entity) used
    /// in the [`InstanceKey`]; `self_entity` is what the guest sees as its own
    /// entity and the target a `NULL`-addressed edit resolves to. The two
    /// differ only for stage-scoped behaviors, where `self_entity` is `NULL`.
    fn attach_inner(
        &mut self,
        world: &mut World,
        key_entity: WireEntity,
        self_entity: WireEntity,
        stage_scoped: bool,
        uuid: &str,
        context: Option<&str>,
    ) -> Result<(), ModuleError> {
        if let Some(context) = context {
            if context.len() > idtx_behavior_abi::MAX_BEHAVIOR_CONTEXT_BYTES {
                return Err(ModuleError::HostLimit("behavior context"));
            }
            serde_json::from_str::<serde_json::Value>(context).map_err(|error| {
                ModuleError::LinkFailed(format!("invalid behavior context: {error}"))
            })?;
        }
        let key = InstanceKey {
            entity: key_entity,
            uuid: uuid.to_owned(),
        };
        if self.instances.len() >= self.limits.work.instances && !self.instances.contains_key(&key)
        {
            return Err(ModuleError::HostLimit("live instances"));
        }
        let id = self
            .next_instance_id
            .checked_add(1)
            .ok_or(ModuleError::HostLimit("instance identities"))?;
        let module = self
            .modules
            .get(uuid)
            .ok_or_else(|| ModuleError::LinkFailed(format!("no module {uuid}")))?;

        let mut state = HostState::with_work(
            module.manifest.capabilities,
            self_entity,
            // Per-behavior cap clamps the host default (clamp-only): a manifest
            // may only LOWER its linear-memory budget, never raise it.
            store_limits(module.manifest.memory_cap(self.limits.memory_cap)),
            self.work_budget
                .child(self.limits.work.queue_items, self.limits.work.queue_bytes),
            self.limits.work,
        );
        // Scene-query backend (raycast / arc-cast / prim-type query), shared by
        // every instance; cheap `Arc` clone.
        state.scene_backend.clone_from(&self.scene_backend);
        state.behavior_context = context.map(str::to_owned);
        state.spawn_generation = self.spawn_generation;
        // Shared widget-interaction state (registry + markers + pending), so a
        // panel behavior's `register` and a pointer behavior's `handle` see one
        // table; cheap `Arc` clone.
        state.widget = Some(self.widget.clone());
        state.input_capture = self.input_capture.clone();
        state.input_owner_key = uuid.to_owned();
        // Network-egress policy for the `net` capability (deny-by-default SSRF
        // gate), shared by every instance.
        state.network_policy = self.network_policy.clone();
        state.auth_scopes = self.auth_scopes.clone();
        state.experience_state = self.experience_state.clone();
        state.time_code = self.time_code;
        state.camera_pose = self.camera_pose;
        state.camera_transform = self.camera_transform;
        state.xr_snapshot = self.xr_snapshot.clone();
        state.xr_recenter = self.xr_recenter;
        // Seed the action-id resolution table from the router's map for
        // every action the manifest declares.
        for decl in &module.manifest.inputs {
            let id = self.router.resolve(&decl.action);
            state.action_ids.insert(decl.action.clone(), id.0);
        }

        let (store, instance) = module.instantiate(&self.engine, state, self.limits)?;
        let mut budget = DispatchBudget::new(self.limits.work);
        // Replacing an existing attachment on this `(entity, uuid)`: tear the
        // prior instance down through its terminal lifecycle (+ guest
        // destructor) before the new one takes the slot, instead of letting the
        // `insert` below drop its Wasmtime store silently and skip teardown.
        if let Some(old) = self.instances.remove(&key) {
            self.instance_order.remove(&old.id);
            Self::teardown_detached(old, world, self.limits, &mut budget);
        }
        self.pending_deliveries.retain(|p| !p.delivery.cancelled());
        let mut live = LiveInstance {
            initial_stage_events: VecDeque::new(),
            stage_initialized: false,
            id,
            uuid: uuid.to_owned(),
            store,
            instance,
            stage_scoped,
            lifecycle: Lifecycle::PendingAttach,
            yielded: false,
            last_hook_us: 0,
            peak_hook_us: 0,
            traps: 0,
            startup_error: None,
            interrupt_traps: 0,
        };
        // Fire on_attach immediately (both profiles, §5.4).
        if Self::call_noarg(
            &mut live,
            world,
            self.limits,
            &mut budget,
            "_idtx_on_attach",
        ) != HookCall::Deferred
        {
            live.lifecycle = Lifecycle::Attached;
        }
        self.next_instance_id = id;
        self.instance_order.insert(id, key.clone());
        self.instances.insert(key, live);
        self.finish_dispatch(&budget);
        Ok(())
    }

    /// Detach an instance, firing `on_stop` (if it was playing) then
    /// `on_detach`.
    pub fn detach(&mut self, world: &mut World, entity: WireEntity, uuid: &str) {
        let mut budget = DispatchBudget::new(self.limits.work);
        let key = InstanceKey {
            entity,
            uuid: uuid.to_owned(),
        };
        if let Some(live) = self.instances.remove(&key) {
            self.instance_order.remove(&live.id);
            Self::teardown_detached(live, world, self.limits, &mut budget);
        }
        self.clear_widget_handlers(entity);
        self.pending_deliveries.retain(|p| !p.delivery.cancelled());
        self.finish_dispatch(&budget);
    }

    /// Destroy an instance — the terminal teardown when its entity is
    /// removed from the stage. Fires the full end-of-life sequence
    /// `on_stop` (if playing) → `on_detach` → `on_destroy`, then drops it.
    ///
    /// Distinct from [`Self::detach`], which is the "behavior component
    /// removed" path: `on_destroy` additionally signals that the owning
    /// object is going away for good (§5.4).
    pub fn destroy(&mut self, world: &mut World, entity: WireEntity, uuid: &str) {
        let mut budget = DispatchBudget::new(self.limits.work);
        let key = InstanceKey {
            entity,
            uuid: uuid.to_owned(),
        };
        if let Some(live) = self.instances.remove(&key) {
            self.instance_order.remove(&live.id);
            Self::teardown_destroyed(live, world, self.limits, &mut budget);
        }
        self.clear_widget_handlers(entity);
        self.pending_deliveries.retain(|p| !p.delivery.cancelled());
        self.finish_dispatch(&budget);
    }

    /// Drop every widget handler registered for `entity` (its panel behavior is
    /// detaching / destroyed), so a recycled entity slot never inherits a stale
    /// handler. No-op when no widget state is present.
    fn clear_widget_handlers(&mut self, entity: WireEntity) {
        let has_live_owner = self.has_instance_for_entity(entity);
        if let Ok(mut w) = self.widget.lock() {
            if has_live_owner {
                w.prune_expired();
            } else {
                w.clear_panel(entity);
            }
        }
    }

    /// Tear down **every** live instance through the full end-of-life sequence
    /// (`on_stop` if playing → `on_detach` → `on_destroy`) and drop each guest
    /// instance via `_idtx_drop_instance`, then empty the table.
    ///
    /// This is the host-shutdown / host-replacement path. Dropping a
    /// [`BehaviorHost`] (or overwriting it, e.g. via a session's
    /// `set_behavior_limits`) otherwise drops the Wasmtime stores directly,
    /// which skips the guest destructors and terminal hooks. `Drop` cannot run
    /// them because the hooks need `&mut World`, so an owner that is tearing the
    /// host down must route through this method first.
    pub fn detach_all(&mut self, world: &mut World) {
        let limits = self.limits;
        let mut budget = DispatchBudget::new(limits.work);
        for (_key, live) in self.instances.drain() {
            Self::teardown_destroyed(live, world, limits, &mut budget);
        }
        self.instance_order.clear();
        self.pending_deliveries.clear();
        if let Ok(mut focus) = self.input_capture.lock() { focus.clear(); }
        if let Ok(mut widget) = self.widget.lock() {
            widget.clear();
        }
        self.finish_dispatch(&budget);
    }

    /// Run the "behavior removed, entity persists" teardown on an owned,
    /// already-removed instance: `on_stop` (if started) → `on_detach`, then drop
    /// the guest instance via `_idtx_drop_instance` while its store + host
    /// imports are still valid. No-op `_idtx_drop_instance` for modules that
    /// predate the export (`call_noarg` skips a missing export). Shared by
    /// [`Self::detach`] and the [`Self::attach`] overwrite path.
    fn teardown_detached(
        mut live: LiveInstance,
        world: &mut World,
        limits: Limits,
        budget: &mut DispatchBudget,
    ) {
        live.initial_stage_events.clear();
        Self::stop_if_started(&mut live, world, limits, budget);
        Self::call_noarg(&mut live, world, limits, budget, "_idtx_on_detach");
        Self::drop_guest(&mut live, world, limits);
    }

    /// Run the "object gone for good" teardown on an owned, already-removed
    /// instance: `on_stop` (if started) → `on_detach` → `on_destroy`, then drop
    /// the guest instance via `_idtx_drop_instance`. Shared by [`Self::destroy`]
    /// and [`Self::detach_all`].
    fn teardown_destroyed(
        mut live: LiveInstance,
        world: &mut World,
        limits: Limits,
        budget: &mut DispatchBudget,
    ) {
        live.initial_stage_events.clear();
        Self::stop_if_started(&mut live, world, limits, budget);
        Self::call_noarg(&mut live, world, limits, budget, "_idtx_on_detach");
        Self::call_noarg(&mut live, world, limits, budget, "_idtx_on_destroy");
        Self::drop_guest(&mut live, world, limits);
    }

    /// Invoke the guest destructor `_idtx_drop_instance`, which must always run
    /// before the Wasmtime store is dropped (§5.4) so guest-side resources are
    /// released. The preceding terminal hooks (`on_stop`/`on_detach`/
    /// `on_destroy`) share the caller's aggregate dispatch budget and can exhaust
    /// it or yield the instance; `invoke` then short-circuits on `live.yielded`
    /// *before* it even consults a budget, so a dedicated budget alone would not
    /// be enough. Clear the yield gate (teardown is one-shot — there is no next
    /// frame to defer into) and arm a fresh budget so the destructor cannot be
    /// silently skipped. A faulted guest (`traps > 0`) is still skipped by
    /// `invoke`: re-entering a trapped store is unsafe and its memory is
    /// reclaimed when the store drops.
    fn drop_guest(live: &mut LiveInstance, world: &mut World, limits: Limits) {
        live.yielded = false;
        let mut budget = DispatchBudget::new(limits.work);
        Self::call_noarg(live, world, limits, &mut budget, "_idtx_drop_instance");
    }

    /// Number of live instances.
    #[must_use]
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    /// Whether any live behavior instance is attached to `entity`.
    ///
    /// The session uses this to decide single-authority for XR controller /
    /// pointer poses: when an authored `*_controller_sync` / `*_aim_sync`
    /// behavior owns a hand's prim, the session suppresses its own hydrated
    /// pose write for that prim so the two drivers never conflict.
    #[must_use]
    pub fn has_instance_for_entity(&self, entity: WireEntity) -> bool {
        self.instances.keys().any(|key| key.entity == entity)
    }

    #[must_use]
    pub fn has_live_instance_for_entity(&self, entity: WireEntity) -> bool {
        self.instances
            .iter()
            .any(|(key, live)| key.entity == entity && live.traps == 0)
    }

    #[must_use]
    pub fn has_ready_instance_for_entity(&self, entity: WireEntity) -> bool {
        self.instances.iter().any(|(key, live)| {
            key.entity == entity
                && live.traps == 0
                && !live.yielded
                && live.lifecycle != Lifecycle::PendingAttach
        })
    }

    #[must_use]
    pub fn has_instance(&self, entity: WireEntity, uuid: &str) -> bool {
        self.instances.contains_key(&InstanceKey {
            entity,
            uuid: uuid.into(),
        })
    }

    /// Release an authored module once its last live binding is gone.
    pub fn release_unused_module(&mut self, uuid: &str) {
        if !self.instances.keys().any(|key| key.uuid == uuid) {
            self.modules.remove(uuid);
        }
    }

    /// Schedule post-attach lifecycle for one newly bound stage behavior.
    /// Delivery uses the next frame's clocks and aggregate dispatch budget.
    pub fn initialize_stage_binding(&mut self, entity: WireEntity, uuid: &str, playing: bool) {
        if let Some(live) = self.instances.get_mut(&InstanceKey {
            entity,
            uuid: uuid.into(),
        }) && live.stage_scoped
            && !live.stage_initialized
        {
            live.stage_initialized = true;
            live.initial_stage_events = VecDeque::from([StageEvent::Loaded]);
            if playing {
                live.initial_stage_events.push_back(StageEvent::PlayStart);
            }
        }
    }

    fn retire_missing_entities(&mut self, world: &mut World, budget: &mut DispatchBudget) {
        let removed: Vec<_> = self
            .instances
            .keys()
            .filter(|key| {
                key.entity != WireEntity::NULL
                    && crate::stage_access::resolve_live(world, key.entity).is_err()
            })
            .cloned()
            .collect();
        for key in removed {
            if let Some(mut live) = self.instances.remove(&key) {
                self.instance_order.remove(&live.id);
                live.yielded = false;
                Self::teardown_destroyed(live, world, self.limits, budget);
            }
            self.clear_widget_handlers(key.entity);
        }
        self.pending_deliveries
            .retain(|pending| !pending.delivery.cancelled());
    }

    /// Refresh the per-frame input snapshot from raw device state, advancing
    /// hold/tap trigger timers by `dt` seconds and recording this frame's
    /// button edge events for `_idtx_on_input` delivery.
    pub fn update_input(&mut self, raw: &crate::input::RawInput, dt: f32) {
        let snap = self.router.snapshot(raw, dt);
        // Diff against the previous frame's snapshot (still in `self.input`)
        // before overwriting it.
        self.pending_input_events = snap.phase_changes(&self.input);
        self.input = snap;
    }

    /// Drain every instance's pending playback-transport commands.
    ///
    /// A command posted with [`WireEntity::NULL`] as its target addresses the
    /// posting instance's binding entity, so it is resolved to that entity here;
    /// explicit targets pass through unchanged. The session calls this once
    /// per frame after [`Self::run_frame`] and applies each command to the
    /// addressed entity's `AnimClipPlayer` (the host has no knowledge of that
    /// higher-layer component, so it only surfaces the commands).
    #[must_use]
    pub fn drain_playback_commands(&mut self) -> Vec<crate::playback::PlaybackCommand> {
        self.drain_playback_requests().into_iter().map(|r| r.command).collect()
    }

    pub fn drain_playback_requests(&mut self) -> Vec<crate::playback::PlaybackRequest> {
        let mut out = Vec::new();
        for (id, key) in &self.instance_order {
            let Some(live) = self.instances.get_mut(key) else { continue };
            for mut cmd in live.store.data_mut().playback.drain() {
                if cmd.target.is_null() {
                    cmd.target = key.entity;
                }
                out.push(crate::playback::PlaybackRequest {
                    command: cmd, origin: key.entity, attachment: *id,
                    sequence: self.next_playback_sequence,
                });
                self.next_playback_sequence = self.next_playback_sequence.wrapping_add(1);
            }
        }
        out
    }

    /// Queue a runtime notification through the normal budgeted dispatcher.
    /// Pin its recipients now: detach/reattach cannot inherit old completions.
    pub fn post_playback_event(&mut self, target: WireEntity, attachment: Option<u64>,
        name: &str, payload: &[u8]) -> Result<(), idtx_behavior_abi::ErrorCode>
    {
        use crate::work::RetainedSize;
        if target.is_null() || payload.len() > idtx_behavior_abi::MAX_EVENT_PAYLOAD_BYTES {
            return Err(idtx_behavior_abi::ErrorCode::InvalidArgument);
        }
        for (id, key) in &self.instance_order {
            if key.entity != target || attachment.is_some_and(|a| a != *id) { continue; }
            let Some(live) = self.instances.get(key) else { continue };
            if !live.store.data().capabilities.events { continue; }
            let event = crate::Event { target, name: name.to_owned(), payload: payload.to_vec() };
            let reservation = self.work_budget.reserve(event.retained_size())?;
            self.pending_deliveries.push_back(PendingDelivery {
                delivery: Delivery::Event(Queued { value: event, reservation }),
                after: id.saturating_sub(1), through: *id,
            });
        }
        Ok(())
    }

    /// Drain every instance's pending session-layer edits, resolving a
    /// NULL command target to the posting instance's own entity. The session
    /// calls this once per frame after [`Self::run_frame`] and applies each
    /// edit to the session layer + re-projects (the host has no stage handle,
    /// so it only surfaces the deferred edits).
    #[must_use]
    pub fn drain_session_edits(&mut self) -> Vec<crate::session_edit::SessionEditCommand> {
        let mut out = Vec::new();
        for live in self.instances.values_mut() {
            let self_entity = live.store.data().self_entity;
            for mut cmd in live.store.data_mut().session_edits.drain() {
                if cmd.target.is_null() {
                    cmd.target = self_entity;
                }
                out.push(cmd);
            }
        }
        out
    }

    /// Drain every instance's pending level-transition requests. The
    /// session calls this once per frame after [`Self::run_frame`] and surfaces
    /// the result for the runtime driver to poll, which performs the actual
    /// session swap (the host has no level/session handle, so it only surfaces
    /// the deferred requests). A request carries only `(target, spawn)` — no
    /// entity — so there is no NULL-target resolution.
    #[must_use]
    pub fn drain_level_transitions(
        &mut self,
    ) -> Vec<crate::level_transition::LevelTransitionRequest> {
        let mut out = Vec::new();
        for live in self.instances.values_mut() {
            out.extend(live.store.data_mut().level_transitions.drain());
        }
        out
    }

    /// Drain every instance's pending physics-control commands (grab
    /// suspend/resume), resolving a NULL command target to the posting
    /// instance's own entity. The session calls this once per frame after
    /// [`Self::run_frame`] and applies each command to its physics subsystem
    /// before the physics tick (the host has no stage/physics handle, so it only
    /// surfaces the deferred commands).
    #[must_use]
    pub fn drain_physics_control_commands(
        &mut self,
    ) -> Vec<crate::physics_control::PhysicsControlCommand> {
        let mut out = Vec::new();
        for live in self.instances.values_mut() {
            let self_entity = live.store.data().self_entity;
            for mut cmd in live.store.data_mut().physics_control.drain() {
                if cmd.target.is_null() {
                    cmd.target = self_entity;
                }
                out.push(cmd);
            }
        }
        out
    }

    /// Drain every instance's pending transient overlay / render commands
    /// Resolving a NULL command target to the posting instance's own
    /// entity. The session calls this once per frame after [`Self::run_frame`]
    /// and feeds the commands into the render world's per-prim highlight /
    /// material-override / post-process state — they never author into USD and
    /// do not survive the frame.
    #[must_use]
    pub fn drain_overlay_commands(&mut self) -> Vec<crate::overlay::OverlayCommand> {
        let mut out = Vec::new();
        for live in self.instances.values_mut() {
            let self_entity = live.store.data().self_entity;
            for mut cmd in live.store.data_mut().overlay.drain() {
                if cmd.target.is_null() {
                    cmd.target = self_entity;
                }
                out.push(cmd);
            }
        }
        out
    }
    /// frame (before [`Self::run_frame`]) with the latest tracked poses;
    /// each behavior tick then reads it through the `xr_hmd` import
    /// without a live FFI into the XR platform on the hot path.
    pub fn set_xr_snapshot(&mut self, snapshot: XrSnapshot) {
        self.xr_snapshot = snapshot;
    }

    /// Set the current play-space recenter offset (stage-space ground vector),
    /// seeded into each store before the next [`Self::run_frame`] and read by a
    /// pawn-root behavior through the `idtx_xr_get_recenter` import. Set beside
    /// [`Self::set_xr_snapshot`]; `ZERO` disables the recenter.
    pub fn set_xr_recenter(&mut self, recenter: glam::Vec3) {
        self.xr_recenter = recenter;
    }

    /// Set the active view/player camera pose (stage space), seeded into each
    /// store before the next [`Self::run_frame`] and read through the
    /// `idtx_camera_pose` import. The session sets this from the desktop
    /// orbit/player camera (or the HMD in XR) so a behavior can raycast from the
    /// viewpoint. A default (`valid == 0`) pose disables it.
    pub fn set_camera_pose(&mut self, pose: idtx_behavior_abi::CameraPose) {
        self.camera_pose = pose;
    }

    /// Set the active view/player camera world transform (stage space), seeded
    /// into each store beside [`Self::set_camera_pose`] and read through the
    /// `idtx_camera_transform` import. The session sets this from the same
    /// camera matrix the pose is derived from so a behavior can rigidly attach a
    /// grabbed prim to the camera frame. `IDENTITY` disables it.
    pub fn set_camera_transform(&mut self, transform: glam::Mat4) {
        self.camera_transform = transform;
    }

    /// Set the current time-code (fed into hooks).
    pub fn set_time_code(&mut self, tc: f64) {
        self.time_code = tc;
    }

    /// Drive one frame of the dispatch loop (§5.4).
    ///
    /// Order: `on_update` → `on_late_update` (playing), `on_editor_update`
    /// (authoring), then drain events → `on_event`. `on_start` fires on
    /// the first playing frame for each instance.
    /// Pending callbacks retain their continuation on budget exhaustion. Frame
    /// and input hooks are best-effort; an already-invoked hook is not retried.
    ///
    /// # Panics
    ///
    /// Panics if an internal callback clears the active HTTP completion context.
    pub fn run_frame(&mut self, world: &mut World, dt: f32, play: PlayState) {
        let mut budget = DispatchBudget::new(self.limits.work);
        self.retire_missing_entities(world, &mut budget);
        // Seed shared frame state into each store's HostState.
        let input = self.input.clone();
        let xr_snapshot = self.xr_snapshot.clone();
        let xr_recenter = self.xr_recenter;
        let camera_pose = self.camera_pose;
        let camera_transform = self.camera_transform;
        let time_code = self.time_code;
        // This frame's input edge events (delivered to `_idtx_on_input` before
        // `on_update`, playing only). Taken so they fire exactly once.
        let input_events = std::mem::take(&mut self.pending_input_events);

        // Snapshot the keys so we can iterate + mutate the map's values.
        let keys: Vec<InstanceKey> = self.instance_order.values().cloned().collect();
        // Normal frames keep HTTP → update → event order, with spawn-completion
        // delivery as a 4th phase. Under overload, rotate all four phases for
        // first access to the budget. Per-phase instance cursors advance only when
        // that phase can run, so phase/key cycle lengths cannot repeatedly favour
        // the same subset of instances.
        let first_phase = if self.frame_exhausted || !self.pending_deliveries.is_empty() {
            self.frame_round % 4
        } else {
            0
        };
        self.pending_deliveries.retain(|p| !p.delivery.cancelled());

        // Seed this frame's shared state into every live instance's HostState
        // BEFORE any hook runs this frame. The async HTTP completion sweep in
        // Step 0 fires handlers that are explicitly passed a `Ctx`; without this
        // pre-pass they would observe the previous frame's `delta_seconds`,
        // time-code, input, XR, and camera state (or the defaults on the first
        // frame).
        for key in &keys {
            let Some(live) = self.instances.get_mut(key) else {
                continue;
            };
            if live.traps > 0 {
                continue; // disabled after a trap
            }
            live.yielded = false;
            let data = live.store.data_mut();
            data.delta_seconds = dt;
            data.time_code = time_code;
            data.input = input.clone();
            data.xr_snapshot = xr_snapshot.clone();
            data.xr_recenter = xr_recenter;
            data.camera_pose = camera_pose;
            data.camera_transform = camera_transform;
            data.input_player = self.scene_backend.as_ref().and_then(|b| b.input_player(world, bevy_ecs::entity::Entity::from_bits(key.entity.to_bits())))
                .map_or(WireEntity::NULL, |e| WireEntity::from_bits(e.to_bits()));
        }

        // All interaction policies observe raw input and submit ownership before
        // ANY input/update consumer runs. Sorting claims at this boundary makes
        // arbitration independent of the update scheduler's rotating order.
        // Overload alternates a bounded capture sweep with a consumer-only
        // frame. This also lets a guest yielded by its capture hook make normal
        // progress with a one-hook/fuel/time budget. Retained claims still commit
        // before input on that frame, with the same deterministic arbitration.
        let skip_capture = std::mem::take(&mut self.capture_exhausted);
        if play == PlayState::Playing && !skip_capture {
            let mut capture_budget = budget.capture_slice();
            let mut capture_keys = keys.clone();
            if !capture_keys.is_empty() {
                let start = self.capture_round % capture_keys.len();
                capture_keys.rotate_left(start);
            }
            self.capture_round = self.capture_round.wrapping_add(1);
            for key in &capture_keys {
                if let Some(live) = self.instances.get_mut(key) {
                    // Missing capture hooks must cost no recipient visits:
                    // otherwise a small dispatch budget can starve every legacy
                    // consumer before the rotating normal phases get a turn.
                    if live.instance.get_func(&mut live.store, "_idtx_on_input_capture").is_none() { continue; }
                    if live.lifecycle != Lifecycle::Started {
                        let outcome = Self::call_noarg(live, world, self.limits, &mut capture_budget, "_idtx_on_start");
                        if matches!(outcome, HookCall::Deferred | HookCall::Failed | HookCall::Disabled) { continue; }
                        live.lifecycle = Lifecycle::Started;
                    }
                    live.store.data_mut().input_capture_phase = true;
                    Self::call_noarg(live, world, self.limits, &mut capture_budget, "_idtx_on_input_capture");
                    live.store.data_mut().input_capture_phase = false;
                }
            }
            self.capture_exhausted = capture_budget.exhausted();
            budget.return_unused(capture_budget);
        }
        let backend = self.scene_backend.clone();
        if let Ok(mut focus) = self.input_capture.lock() {
            if play != PlayState::Playing { focus.clear(); }
            focus.commit(&input, |c| {
                let target = bevy_ecs::entity::Entity::from_bits(c.target.to_bits());
                let owner = bevy_ecs::entity::Entity::from_bits(c.owner.to_bits());
                let tracked = match c.source {
                    idtx_behavior_abi::PointerSource::Left => xr_snapshot.get(idtx_behavior_abi::XrSpace::LeftAim).valid != 0,
                    idtx_behavior_abi::PointerSource::Right => xr_snapshot.get(idtx_behavior_abi::XrSpace::RightAim).valid != 0,
                    _ => true,
                };
                tracked && world.get_entity(target).is_ok() && (c.owner.is_null() || world.get_entity(owner).is_ok())
                    && (c.player.is_null() || world.get_entity(bevy_ecs::entity::Entity::from_bits(c.player.to_bits())).is_ok())
                    && (!c.active || backend.as_ref().is_none_or(|b| b.input_target_available(world, target)))
            });
        }
        if let Ok(mut widget) = self.widget.lock() { widget.begin_frame(); }

        // Advance only when consumers get a turn; alternating capture frames
        // must not lock the four-phase rotation to one parity forever.
        if !budget.exhausted() { self.frame_round = self.frame_round.wrapping_add(1); }
        for step in 0..4 {
            match (first_phase + step) % 4 {
                0 => self.deliver_http(world, &keys, &mut budget),
                2 => self.deliver_pending(world, &mut budget),
                3 => self.deliver_spawns(world, &keys, &mut budget),
                _ => self.update_instances(world, &keys, dt, play, &input_events, &mut budget),
            }
        }

        // Move this frame's event prefix into bounded, reservation-owning
        // continuations. Events posted by these handlers wait for a later sweep.
        for key in &keys {
            if let Some(live) = self.instances.get_mut(key) {
                for event in live.store.data_mut().events.take_all() {
                    self.pending_deliveries.push_back(PendingDelivery {
                        delivery: Delivery::Event(event),
                        after: 0,
                        through: self.next_instance_id,
                    });
                }
            }
        }

        // Step 7: dispatch widget clicks queued this frame by `idtx_widget_handle`
        // (ABI 3). Each is delivered to the target panel's instances via the same
        // `_idtx_on_event` seam, seeding the widget event so the always-linked
        // `idtx_widget_event_*` reads (and the macro dispatcher) recover it.
        let pending = self
            .widget
            .lock()
            .map(|mut w| w.take_pending())
            .unwrap_or_default();
        for ev in pending {
            self.pending_deliveries.push_back(PendingDelivery {
                delivery: Delivery::Widget(ev),
                after: 0,
                through: self.next_instance_id,
            });
        }
        self.deliver_pending(world, &mut budget);
        self.frame_exhausted = budget.exhausted() || self.capture_exhausted;
        self.finish_dispatch(&budget);
    }

    fn update_instances(
        &mut self,
        world: &mut World,
        keys: &[InstanceKey],
        dt: f32,
        play: PlayState,
        input_events: &[(InputActionId, InputPhase)],
        budget: &mut DispatchBudget,
    ) {
        if budget.exhausted() {
            return;
        }
        let mut keys = keys.to_vec();
        if !keys.is_empty() {
            let start = self.update_round % keys.len();
            keys.rotate_left(start);
        }
        self.update_round = self.update_round.wrapping_add(1);
        let limits = self.limits;
        for key in &keys {
            let Some(live) = self.instances.get_mut(key) else {
                continue;
            };
            if live.traps > 0
                || live.startup_error.is_some()
                || live.yielded
                || live.lifecycle == Lifecycle::PendingAttach
            {
                continue;
            }
            match play {
                PlayState::Playing => {
                    if live.lifecycle != Lifecycle::Started {
                        let outcome =
                            Self::call_noarg(live, world, limits, budget, "_idtx_on_start");
                        if outcome == HookCall::Deferred
                            || outcome == HookCall::Failed
                            || live.traps > 0
                        {
                            continue;
                        }
                        live.lifecycle = Lifecycle::Started;
                    }
                    // Input edges precede updates; absent exports are no-ops.
                    for &(id, phase) in input_events {
                        if budget.exhausted() || live.traps > 0 || live.yielded {
                            break;
                        }
                        if live.store.data().input_phase(id, idtx_behavior_abi::PointerSource::Unspecified) != phase { continue; }
                        Self::call_u32x2(
                            live,
                            world,
                            limits,
                            budget,
                            "_idtx_on_input",
                            id.0,
                            phase.as_u32(),
                        );
                    }
                    Self::call_dt(live, world, limits, budget, "_idtx_on_update", dt);
                    Self::call_dt(live, world, limits, budget, "_idtx_on_late_update", dt);
                }
                PlayState::Authoring => {
                    Self::stop_if_started(live, world, limits, budget);
                    Self::call_dt(live, world, limits, budget, "_idtx_on_editor_update", dt);
                }
            }
        }
    }

    fn deliver_http(
        &mut self,
        world: &mut World,
        keys: &[InstanceKey],
        budget: &mut DispatchBudget,
    ) {
        if budget.exhausted() {
            return;
        }
        let mut keys = keys.to_vec();
        if !keys.is_empty() {
            let start = self.http_round % keys.len();
            keys.rotate_left(start);
        }
        self.http_round = self.http_round.wrapping_add(1);
        let limits = self.limits;
        for key in &keys {
            let Some(live) = self.instances.get_mut(key) else {
                continue;
            };
            if live.traps > 0 {
                continue;
            }
            if live.lifecycle == Lifecycle::PendingAttach
                && Self::call_noarg(live, world, limits, budget, "_idtx_on_attach")
                    != HookCall::Deferred
            {
                live.lifecycle = Lifecycle::Attached;
            }
            while let Some(event) = live.initial_stage_events.pop_front() {
                if Self::call_u32(
                    live,
                    world,
                    limits,
                    budget,
                    "_idtx_on_stage_event",
                    event.as_u32(),
                ) == HookCall::Deferred
                {
                    live.initial_stage_events.push_front(event);
                    break;
                }
            }
            while !budget.exhausted()
                && live.traps == 0
                && !live.yielded
                && live.lifecycle != Lifecycle::PendingAttach
            {
                let completion = live
                    .store
                    .data()
                    .http_inbox
                    .lock()
                    .ok()
                    .and_then(|mut q| q.pop());
                let Some(Queued {
                    value: completion,
                    reservation,
                }) = completion
                else {
                    break;
                };
                let request_id = completion.request_id;
                let status = u32::from(completion.status);
                live.store.data_mut().set_current_http(Some(completion));
                let outcome = Self::call_u32x2(
                    live,
                    world,
                    limits,
                    budget,
                    "_idtx_on_http_response",
                    request_id,
                    status,
                );
                let completion = live
                    .store
                    .data_mut()
                    .take_current_http()
                    .expect("installed completion");
                if outcome == HookCall::Deferred {
                    if let Ok(mut q) = live.store.data().http_inbox.lock() {
                        q.return_front(Queued {
                            value: completion,
                            reservation,
                        });
                    }
                    break;
                }
            }
        }
    }

    /// Deliver terminal callback-mode spawn completions to each instance's
    /// `_idtx_on_spawn_result` export — the spawn analogue of [`Self::deliver_http`]
    /// over a plain per-store queue. Shares the per-frame [`DispatchBudget`] (an
    /// exhausted frame leaves completions queued for the next frame) and the same
    /// per-instance disabled/yield/`PendingAttach` guards. The 16-byte
    /// `SpawnResult` is passed as scalar args `(ticket, state, entity, error)` —
    /// no read-back import.
    fn deliver_spawns(
        &mut self,
        world: &mut World,
        keys: &[InstanceKey],
        budget: &mut DispatchBudget,
    ) {
        if budget.exhausted() {
            return;
        }
        let mut keys = keys.to_vec();
        if !keys.is_empty() {
            let start = self.spawn_round % keys.len();
            keys.rotate_left(start);
        }
        self.spawn_round = self.spawn_round.wrapping_add(1);
        let limits = self.limits;
        for key in &keys {
            let Some(live) = self.instances.get_mut(key) else {
                continue;
            };
            if live.traps > 0 {
                continue;
            }
            while !budget.exhausted()
                && live.traps == 0
                && !live.yielded
                && live.lifecycle != Lifecycle::PendingAttach
            {
                let Some(completion) = live.store.data_mut().pop_spawn_completion() else {
                    break;
                };
                let (ticket, result) = (completion.ticket, completion.result);
                // `(u64, u32, u64, u32)` satisfies `WasmParams`.
                let outcome = Self::call(
                    live,
                    world,
                    limits,
                    budget,
                    "_idtx_on_spawn_result",
                    (ticket, result.state, result.entity.to_bits(), result.error_code),
                );
                if outcome == HookCall::Deferred {
                    // Budget/yield deferral: put it back at the front and retry
                    // this instance on a later phase/frame (same as `deliver_http`).
                    live.store
                        .data_mut()
                        .return_spawn_completion(completion);
                    break;
                }
                // Delivered (or the export is missing / instance disabled): drop
                // the terminal record. The completion retains its shared credit
                // until this iteration ends, even if the callback polled itself.
                live.store.data_mut().consume_spawn_result(ticket);
            }
        }
    }

    fn deliver_pending(&mut self, world: &mut World, budget: &mut DispatchBudget) {
        while !budget.exhausted() {
            let Some(pending) = self.pending_deliveries.front_mut() else {
                break;
            };
            if pending.delivery.cancelled() || pending.after >= pending.through {
                self.pending_deliveries.pop_front();
                continue;
            }
            if !budget.visit() {
                break;
            }
            let next = self
                .instance_order
                .range((Excluded(pending.after), Included(pending.through)))
                .next()
                .map(|(&id, key)| (id, key.clone()));
            let Some((id, key)) = next else {
                self.pending_deliveries.pop_front();
                continue;
            };
            let target = pending.delivery.target();
            if !target.is_null() && crate::stage_access::resolve_live(world, target).is_err() {
                self.pending_deliveries.pop_front();
                continue;
            }
            if let Delivery::Widget(ev) = &pending.delivery {
                let element = (ev.value.kind != idtx_behavior_abi::WidgetEventKind::PanelClick).then_some(ev.value.element.as_str());
                if self.scene_backend.as_ref().is_some_and(|b| !b.widget_event_valid(world,
                    bevy_ecs::entity::Entity::from_bits(target.to_bits()), element)) {
                    self.pending_deliveries.pop_front();
                    continue;
                }
            }
            let matches = match &pending.delivery {
                Delivery::Event(_) => target.is_null() || target == key.entity,
                Delivery::Widget(_) => target == key.entity,
            };
            if matches && let Some(live) = self.instances.get_mut(&key) {
                match &pending.delivery {
                    Delivery::Event(ev) => live
                        .store
                        .data_mut()
                        .set_current_event(Some(ev.value.clone())),
                    Delivery::Widget(ev) => live.store.data_mut().set_current_widget_event(Some(
                        crate::CurrentWidgetEvent {
                            source: ev.value.source,
                            element: ev.value.element.clone(),
                            uv: ev.value.uv,
                            kind: ev.value.kind,
                        },
                    )),
                }
                // The recipient visit above already paid for this lookup.
                let outcome = Self::invoke(live, world, self.limits, budget, "_idtx_on_event", ());
                live.store.data_mut().set_current_event(None);
                live.store.data_mut().set_current_widget_event(None);
                if outcome == HookCall::Deferred {
                    break;
                }
            }
            pending.after = id;
            if pending.after >= pending.through {
                self.pending_deliveries.pop_front();
            }
        }
    }

    /// Fire `on_interact` on the instance(s) bound to `entity`.
    pub fn interact(&mut self, world: &mut World, entity: WireEntity) {
        let limits = self.limits;
        let mut budget = DispatchBudget::new(limits.work);
        self.retire_missing_entities(world, &mut budget);
        let keys: Vec<InstanceKey> = self
            .instances
            .keys()
            .filter(|k| k.entity == entity)
            .cloned()
            .collect();
        for key in keys {
            if let Some(live) = self.instances.get_mut(&key) {
                Self::call_noarg(live, world, limits, &mut budget, "_idtx_on_interact");
            }
        }
        self.finish_dispatch(&budget);
    }

    /// Broadcast a stage-level lifecycle [`StageEvent`] to every
    /// stage-scoped behavior (those attached via [`Self::attach_stage`]) via
    /// the optional `_idtx_on_stage_event(u32)` export.
    ///
    /// Unlike the per-entity transport hooks (`on_start` / `on_stop`),
    /// these events describe the *stage's* coarse lifecycle (load, unload,
    /// play start/stop, pause, resume). Entity-scoped instances never see
    /// them; a stage-scoped instance that does not export the hook is a
    /// silent no-op. The raw [`StageEvent::as_u32`] code is passed as the
    /// single `u32` argument.
    pub fn dispatch_stage_event(&mut self, world: &mut World, event: StageEvent) {
        let limits = self.limits;
        let mut budget = DispatchBudget::new(limits.work);
        self.retire_missing_entities(world, &mut budget);
        let code = event.as_u32();
        // Deliver to every stage-scoped instance. Keyed by `stage_scoped`, not
        // `entity.is_null()`: stage behaviors now key by their distinct
        // `/StageBehavior` scope prim entity, so several can coexist.
        let keys: Vec<InstanceKey> = self
            .instances
            .iter()
            .filter(|(_, live)| live.stage_scoped)
            .map(|(k, _)| k.clone())
            .collect();
        tracing::debug!(
            target: "behavior",
            event = ?event,
            stage_scoped_instances = keys.len(),
            "dispatching stage lifecycle event"
        );
        let mut invoked = 0usize;
        for key in keys {
            if let Some(live) = self.instances.get_mut(&key) {
                if live.traps > 0 {
                    continue; // disabled after a trap
                }
                if event == StageEvent::Unloading {
                    live.initial_stage_events.clear();
                }
                if !live.initial_stage_events.is_empty() {
                    if matches!(event, StageEvent::PlayStart | StageEvent::PlayStop) {
                        live.initial_stage_events.retain(|queued| {
                            !matches!(queued, StageEvent::PlayStart | StageEvent::PlayStop)
                        });
                    }
                    if !live.initial_stage_events.contains(&event)
                        && live.initial_stage_events.len() < 8
                    {
                        live.initial_stage_events.push_back(event);
                    }
                    continue;
                }
                if Self::call_u32(
                    live,
                    world,
                    limits,
                    &mut budget,
                    "_idtx_on_stage_event",
                    code,
                ) == HookCall::Invoked
                {
                    invoked += 1;
                    tracing::debug!(
                        target: "behavior",
                        event = ?event,
                        uuid = %live.uuid,
                        "invoked stage-event hook on wasm behavior"
                    );
                }
            }
        }
        tracing::info!(
            target: "behavior",
            event = ?event,
            invoked,
            "stage lifecycle event delivered to wasm behaviors"
        );
        self.finish_dispatch(&budget);
    }

    // -- hook invocation -------------------------------------------

    /// Fire `on_stop` on `live` iff it had started this play session, then
    /// return to the attached state. Idempotent — a no-op for an instance that
    /// never started (or already stopped), so it is safe to call on every
    /// authoring frame and on detach/destroy.
    fn stop_if_started(
        live: &mut LiveInstance,
        world: &mut World,
        limits: Limits,
        budget: &mut DispatchBudget,
    ) {
        if live.lifecycle != Lifecycle::Started {
            return;
        }
        if Self::call_noarg(live, world, limits, budget, "_idtx_on_stop") != HookCall::Deferred {
            live.lifecycle = Lifecycle::Attached;
        }
    }

    /// Every execution entry uses the same disabled/yielded checks and debits
    /// one batch budget. In particular, late/event/HTTP/terminal hooks cannot
    /// resume an instance that an earlier hook disabled in this dispatch.
    fn call<P: WasmParams>(
        live: &mut LiveInstance,
        world: &mut World,
        limits: Limits,
        budget: &mut DispatchBudget,
        export: &str,
        args: P,
    ) -> HookCall {
        if live.traps > 0 || live.startup_error.is_some() {
            return HookCall::Disabled;
        }
        if !budget.visit() {
            return HookCall::Deferred;
        }
        Self::invoke(live, world, limits, budget, export, args)
    }

    fn invoke<P: WasmParams>(
        live: &mut LiveInstance,
        world: &mut World,
        limits: Limits,
        budget: &mut DispatchBudget,
        export: &str,
        args: P,
    ) -> HookCall {
        if live.traps > 0 || live.startup_error.is_some() {
            return HookCall::Disabled;
        }
        if live.lifecycle == Lifecycle::PendingAttach && export != "_idtx_on_attach" {
            return HookCall::Deferred;
        }
        if !live.initial_stage_events.is_empty()
            && export != "_idtx_on_attach"
            && export != "_idtx_on_stage_event"
        {
            return HookCall::Deferred;
        }
        if live.yielded {
            return HookCall::Deferred;
        }
        let Ok(func): Result<TypedFunc<P, ()>, _> =
            live.instance.get_typed_func(&mut live.store, export)
        else {
            return HookCall::Missing;
        };
        let Some(grant) = budget.arm(&mut live.store, limits) else {
            return HookCall::Deferred;
        };
        let start = std::time::Instant::now();
        // Install the transient world borrow, call, then clear — we can't
        // hold a `&mut HostState` borrow across `func.call` (it needs
        // `&mut Store`), so we thread the pointer explicitly.
        live.store.data_mut().hook_error = None;
        live.store.data_mut().set_world(std::ptr::from_mut(world));
        let result = func.call(&mut live.store, args);
        live.store.data_mut().clear_world();
        let aggregate = budget.finish(&live.store, grant, &result);
        Self::record(live, export, start, result, aggregate);
        if let Some(error) = live.store.data().hook_error {
            tracing::warn!(target: "behavior", uuid = %live.uuid, hook = export, ?error, "behavior hook returned an error");
            if matches!(export, "_idtx_on_attach" | "_idtx_on_start") {
                live.startup_error = Some(error);
                live.store.data_mut().disable_work();
            }
            return HookCall::Failed;
        }
        // An interrupted/trapped startup hook did not complete initialization.
        if matches!(export, "_idtx_on_attach" | "_idtx_on_start") && live.yielded {
            live.startup_error = Some(idtx_behavior_abi::ErrorCode::InvalidArgument);
            live.store.data_mut().disable_work();
            return HookCall::Failed;
        }
        HookCall::Invoked
    }

    fn call_noarg(
        live: &mut LiveInstance,
        world: &mut World,
        limits: Limits,
        budget: &mut DispatchBudget,
        export: &str,
    ) -> HookCall {
        Self::call(live, world, limits, budget, export, ())
    }

    fn call_dt(
        live: &mut LiveInstance,
        world: &mut World,
        limits: Limits,
        budget: &mut DispatchBudget,
        export: &str,
        dt: f32,
    ) -> HookCall {
        Self::call(live, world, limits, budget, export, dt)
    }

    /// Call a `u32`-carrying exported hook (e.g. `_idtx_on_stage_event`).
    ///
    /// The outcome distinguishes actual invocation from a missing export,
    /// disabled instance, or budget/yield deferral.
    fn call_u32(
        live: &mut LiveInstance,
        world: &mut World,
        limits: Limits,
        budget: &mut DispatchBudget,
        export: &str,
        arg: u32,
    ) -> HookCall {
        Self::call(live, world, limits, budget, export, arg)
    }

    /// Call a two-`u32`-argument exported hook (e.g. `_idtx_on_input`).
    fn call_u32x2(
        live: &mut LiveInstance,
        world: &mut World,
        limits: Limits,
        budget: &mut DispatchBudget,
        export: &str,
        a: u32,
        b: u32,
    ) -> HookCall {
        Self::call(live, world, limits, budget, export, (a, b))
    }
    ///
    /// A `Trap::Interrupt` (the hook ran past its *wall-clock* budget) is
    /// treated as **recoverable**: it is most often a benign scheduling
    /// hiccup (the guest thread was descheduled by the OS), so the
    /// instance is allowed to *miss the tick* and keep running. Only if
    /// an instance overruns repeatedly — more than
    /// [`MAX_INTERRUPT_TRAPS`] times — do we promote it to a fatal trap
    /// and disable it, on the assumption it is genuinely runaway.
    ///
    /// Any other trap (a real guest fault: unreachable/panic, OOB
    /// memory, fuel exhaustion, …) is fatal on the first occurrence.
    fn record(
        live: &mut LiveInstance,
        export: &str,
        start: std::time::Instant,
        result: Result<(), wasmtime::Error>,
        aggregate: bool,
    ) {
        let elapsed = start.elapsed().as_micros();
        live.last_hook_us = elapsed;
        live.peak_hook_us = live.peak_hook_us.max(elapsed);

        let Err(err) = result else {
            return;
        };

        if aggregate {
            live.yielded = true;
            tracing::debug!(target: "behavior", uuid = %live.uuid, hook = export,
                "aggregate dispatch budget exhausted; yielding this instance until next frame");
            return;
        }

        // Classify: an epoch-interrupt trap is the recoverable
        // "missed its wall-clock budget this tick" case.
        let is_interrupt = err
            .downcast_ref::<wasmtime::Trap>()
            .is_some_and(|t| *t == wasmtime::Trap::Interrupt);

        match classify_trap(is_interrupt, live.interrupt_traps) {
            TrapOutcome::Tolerate => {
                live.yielded = true;
                live.interrupt_traps += 1;
                // Tolerate the miss: log at WARN and keep the instance
                // alive so a one-off stall doesn't kill the behavior.
                tracing::warn!(
                    target: "behavior",
                    uuid = %live.uuid,
                    hook = export,
                    elapsed_us = elapsed,
                    interrupt_traps = live.interrupt_traps,
                    max = MAX_INTERRUPT_TRAPS,
                    "behavior hook overran its wall-clock budget — \
                     skipping this tick, instance stays enabled"
                );
            }
            TrapOutcome::FatalInterrupt => {
                live.interrupt_traps += 1;
                live.traps += 1;
                tracing::error!(
                    target: "behavior",
                    uuid = %live.uuid,
                    hook = export,
                    interrupt_traps = live.interrupt_traps,
                    "behavior hook repeatedly overran its budget — \
                     disabling instance"
                );
            }
            TrapOutcome::Fatal => {
                live.traps += 1;
                tracing::error!(
                    target: "behavior",
                    uuid = %live.uuid,
                    hook = export,
                    error = %err,
                    "behavior hook trapped — disabling instance"
                );
            }
        }
        if live.traps > 0 {
            live.store.data_mut().disable_work();
        }
    }

    fn finish_dispatch(&mut self, budget: &DispatchBudget) {
        if budget.exhausted() {
            tracing::debug!(target: "behavior", pending = self.pending_deliveries.len(),
                "dispatch budget exhausted; queued deliveries retained, remaining frame/lifecycle hooks skipped");
        }
        #[cfg(feature = "metrics")]
        {
            self.dispatch_metrics = budget.metrics();
        }
    }

    /// Actual work in the last frame / interaction / lifecycle dispatch batch.
    #[cfg(feature = "metrics")]
    #[must_use]
    pub fn dispatch_metrics(&self) -> crate::DispatchMetrics {
        self.dispatch_metrics
    }

    /// Diagnostics for the instance bound to `(entity, uuid)`:
    /// `(last_us, peak_us, traps)`.
    #[must_use]
    pub fn diagnostics(&self, entity: WireEntity, uuid: &str) -> Option<(u128, u128, u32)> {
        let key = InstanceKey {
            entity,
            uuid: uuid.to_owned(),
        };
        self.instances
            .get(&key)
            .map(|l| (l.last_hook_us, l.peak_hook_us, l.traps))
    }

    /// Test hook: push an [`HttpCompletion`](crate::stage_access::HttpCompletion)
    /// directly into an instance's async-HTTP inbox, standing in for a background
    /// worker so the `_idtx_on_http_response` delivery path can be exercised
    /// without real network I/O (the SSRF gate blocks loopback anyway). The
    /// completion is delivered on the next [`Self::run_frame`]. Returns `false`
    /// when no instance is bound to `(entity, uuid)`.
    #[doc(hidden)]
    pub fn inject_http_completion(
        &mut self,
        entity: WireEntity,
        uuid: &str,
        completion: crate::stage_access::HttpCompletion,
    ) -> bool {
        let key = InstanceKey {
            entity,
            uuid: uuid.to_owned(),
        };
        let Some(live) = self.instances.get(&key) else {
            return false;
        };
        if let Ok(mut q) = live.store.data().http_inbox.lock() {
            q.push(completion).is_ok()
        } else {
            false
        }
    }

    /// Test hook: install a terminal callback-mode spawn record and enqueue its
    /// completion for `(entity, uuid)`, standing in for the session's
    /// `settle_runtime_spawns` driver so the `_idtx_on_spawn_result` delivery path
    /// can be exercised without the session. `result` must be terminal
    /// (`Ready` / `Failed`); it is delivered on the next [`Self::run_frame`].
    /// Returns `false` when no instance is bound to `(entity, uuid)` or the queue
    /// budget rejects the record.
    #[doc(hidden)]
    pub fn inject_spawn_completion(
        &mut self,
        entity: WireEntity,
        uuid: &str,
        ticket: u64,
        result: idtx_behavior_abi::SpawnResult,
    ) -> bool {
        let key = InstanceKey {
            entity,
            uuid: uuid.to_owned(),
        };
        let Some(live) = self.instances.get_mut(&key) else {
            return false;
        };
        live.store
            .data_mut()
            .inject_terminal_spawn_completion(ticket, result)
    }
}

/// Build a wasmtime `StoreLimits` capping linear memory.
fn store_limits(memory_cap: usize) -> wasmtime::StoreLimits {
    wasmtime::StoreLimitsBuilder::new()
        .memory_size(memory_cap)
        .build()
}

/// Convenience: encode an ECS entity as the wire handle the host API
/// expects. Re-exported so callers wiring the host into `idtx-scene`
/// don't reach into [`crate::stage_access`].
#[must_use]
pub fn entity_to_wire(e: bevy_ecs::entity::Entity) -> WireEntity {
    to_wire(e)
}

/// What to do with a hook that returned a trap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrapOutcome {
    /// A recoverable wall-clock overrun: skip this tick, keep running.
    Tolerate,
    /// One overrun too many: disable the instance.
    FatalInterrupt,
    /// A genuine guest fault: disable the instance immediately.
    Fatal,
}

/// Decide how to treat a trap given whether it was an epoch interrupt
/// and how many interrupts the instance has already tolerated.
///
/// Pulled out as a pure function so the disable-vs-tolerate policy is
/// unit-testable without standing up a full wasmtime store.
fn classify_trap(is_interrupt: bool, prior_interrupts: u32) -> TrapOutcome {
    if !is_interrupt {
        return TrapOutcome::Fatal;
    }
    // This interrupt is the `(prior_interrupts + 1)`-th. Tolerate up to
    // and including `MAX_INTERRUPT_TRAPS`; the next one is fatal.
    if prior_interrupts < MAX_INTERRUPT_TRAPS {
        TrapOutcome::Tolerate
    } else {
        TrapOutcome::FatalInterrupt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_interrupt_traps_are_fatal_immediately() {
        // A real guest fault disables on the first occurrence regardless
        // of the interrupt history.
        assert_eq!(classify_trap(false, 0), TrapOutcome::Fatal);
        assert_eq!(
            classify_trap(false, MAX_INTERRUPT_TRAPS + 5),
            TrapOutcome::Fatal
        );
    }

    #[test]
    fn interrupts_are_tolerated_up_to_the_cap_then_fatal() {
        // The first `MAX_INTERRUPT_TRAPS` interrupts are tolerated (the
        // instance misses a tick but stays enabled)…
        for prior in 0..MAX_INTERRUPT_TRAPS {
            assert_eq!(
                classify_trap(true, prior),
                TrapOutcome::Tolerate,
                "interrupt #{} should be tolerated",
                prior + 1
            );
        }
        // …and the one *after* the cap is promoted to fatal.
        assert_eq!(
            classify_trap(true, MAX_INTERRUPT_TRAPS),
            TrapOutcome::FatalInterrupt
        );
        assert_eq!(
            classify_trap(true, MAX_INTERRUPT_TRAPS + 1),
            TrapOutcome::FatalInterrupt
        );
    }
}
