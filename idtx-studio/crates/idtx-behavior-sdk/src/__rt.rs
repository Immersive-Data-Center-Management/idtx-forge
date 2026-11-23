//! Guest runtime glue (hidden). Bridges the generated `extern "C"` thunks
//! to the author's behavior instance and declares the raw host import
//! table.
//!
//! On wasm the imports are declared `extern "C"` so a module *links*
//! against a host that provides them. Natively the imports are absent —
//! the safe wrappers in [`crate::entity`] / [`crate::input`] /
//! [`crate::log`] / [`crate::xr`] guard every raw call behind
//! `#[cfg(target_arch = "wasm32")]` and return defaults otherwise, so the
//! SDK (and author unit tests) build and run without a host. Dispatch here
//! constructs the behavior singleton and a `Ctx`, invokes the hook, and
//! logs (does not propagate) any error.

use crate::ctx::Ctx;
use crate::result::Result;

// ---------------------------------------------------------------------------
// Raw host import table (wasm only)
// ---------------------------------------------------------------------------
//
// These are the symbols the host `Linker` provides. Grouped by capability
// bucket. Only `log` + `time` + `entity_self` are always linked; the rest
// are capability-gated host-side (an ungranted import is simply absent, so
// a module referencing it fails to link — compile-time-ish enforcement).

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "idtx")]
unsafe extern "C" {
    pub fn idtx_event_post(target: u64, name: *const u8, name_len: u32, payload: *const u8, payload_len: u32) -> u32;
    pub fn idtx_playback_command(target: u64, clip: *const u8, clip_len: u32, kind: u32, value: f32) -> u32;
    pub fn idtx_relationship_targets(entity: u64, name: *const u8, name_len: u32, out: *mut u8, capacity: u32, result: *mut u8) -> u32;
    pub fn idtx_curve_length(entity: u64, index: u32, out: *mut u8) -> u32;
    pub fn idtx_curve_samples(entity: u64, index: u32, space: u32, distances: *const u8, count: u32, out: *mut u8) -> u32;
    // log (always linked)
    pub fn idtx_log(level: u32, ptr: *const u8, len: u32);

    // Ambient immutable context and hook result reporting (always linked).
    pub fn idtx_behavior_context(out_ptr: *mut u8, out_cap: u32, out_len: *mut u32) -> u32;
    pub fn idtx_hook_result(code: u32);
    // Authored prim data and exact child lookup (scene_query).
    pub fn idtx_prim_custom_data(
        entity: u64,
        key_ptr: *const u8,
        key_len: u32,
        out_ptr: *mut u8,
        out_cap: u32,
        out_len: *mut u32,
    ) -> u32;
    pub fn idtx_entity_named_child(
        entity: u64,
        name_ptr: *const u8,
        name_len: u32,
        out_ptr: *mut u8,
    ) -> u32;
    // Registered reference spawning and ticket reads (stage_edit).
    pub fn idtx_session_spawn_reference(
        parent: u64,
        name_ptr: *const u8,
        name_len: u32,
        prototype_ptr: *const u8,
        prototype_len: u32,
        xform_ptr: *const u8,
        payload_ptr: *const u8,
        payload_len: u32,
        ticket_ptr: *mut u8,
    ) -> u32;
    pub fn idtx_session_spawn_status(ticket: u64, out_ptr: *mut u8) -> u32;

    // time
    pub fn idtx_delta_seconds() -> f32;
    pub fn idtx_time_code() -> f64;
    // Active view/player camera pose (stage space) — writes a 40-byte
    // `abi::CameraPose`. Always linked (no capability).
    pub fn idtx_camera_pose(out_ptr: *mut u8) -> u32;
    // Active view/player camera world transform (stage space) — writes a
    // 64-byte `abi::Mat4`. Always linked (no capability); identity when no
    // camera pose is available this frame.
    pub fn idtx_camera_transform(out_ptr: *mut u8) -> u32;

    // xform (capability: xform_write / scene_query)
    pub fn idtx_entity_self() -> u64;
    pub fn idtx_get_xform(entity: u64, out_ptr: *mut u8) -> u32;
    pub fn idtx_set_xform(entity: u64, in_ptr: *const u8) -> u32;
    pub fn idtx_world_xform(entity: u64, out_ptr: *mut u8) -> u32;

    // session-layer authoring (capability: stage_edit). Deferred:
    // the host enqueues the edit and the session applies + re-projects it
    // after the update hooks. A NULL `target` resolves to the caller's entity.
    pub fn idtx_session_set_xform(target: u64, in_ptr: *const u8) -> u32;
    pub fn idtx_session_set_visibility(target: u64, visible: u32) -> u32;

    // dynamic prim inject/remove (capability: stage_edit). `define_prim`
    // targets the parent (authors a child `name` of type `type_name` under it);
    // `remove_prim` targets the prim to delete. Both route through the session's
    // structural (TOPOLOGY) apply path on drain.
    pub fn idtx_session_define_prim(
        parent: u64,
        type_ptr: *const u8,
        type_len: u32,
        name_ptr: *const u8,
        name_len: u32,
    ) -> u32;
    pub fn idtx_session_remove_prim(target: u64) -> u32;

    // Attach a screen-space HUD element (capability: stage_edit). Atomically
    // defines an `IdtxHud` child `name` under `target` into the session layer
    // and authors its parameters there, in one deferred op. `size_w`/`size_h`
    // are the on-screen pixel size (0,0 = native image size).
    pub fn idtx_hud_attach(
        target: u64,
        name_ptr: *const u8,
        name_len: u32,
        image_ptr: *const u8,
        image_len: u32,
        size_w: i32,
        size_h: i32,
        opacity: f32,
    ) -> u32;

    // Author `idtx:widget:value` (string) onto a widget-field child of a panel
    // (capability: stage_edit). Deferred: resolves the panel-relative child
    // `name` under `target` (NULL = the caller's own panel entity) and authors
    // the string `value` into the session layer in one op. The `idtx:widget:`
    // write drives the session's widget re-hydrate + render patch on drain.
    pub fn idtx_session_set_widget_value(
        target: u64,
        name_ptr: *const u8,
        name_len: u32,
        value_ptr: *const u8,
        value_len: u32,
    ) -> u32;

    // scene query
    pub fn idtx_entity_find(path_ptr: *const u8, path_len: u32, out_ptr: *mut u8) -> u32;
    pub fn idtx_entity_parent(entity: u64, out_ptr: *mut u8) -> u32;
    pub fn idtx_entity_child(entity: u64, index: u32, out_ptr: *mut u8) -> u32;
    pub fn idtx_entity_child_count(entity: u64) -> u32;
    // Resolve a hit gprim up to its collision body (`IdtxCollisionBodyAPI`);
    // writes `Entity::NULL` when the entity is inside no body.
    pub fn idtx_scene_collision_body(entity: u64, out_ptr: *mut u8) -> u32;
    // Read a prim's *effective* visibility — the composed ECS result the
    // renderer draws from (`VisibilityComp.visible`), i.e. the prim's own
    // opinion folded with its ancestor chain, not merely its local override.
    // Writes a single byte (`0` invisible / `1` visible) to `out_ptr`.
    pub fn idtx_get_visibility(entity: u64, out_ptr: *mut u8) -> u32;

    // scene interaction (capability: scene_query). Each reads a POD
    // param struct and writes a POD result; variable-length output (hit path,
    // arc polyline, type-query list) uses the caller-buffer + true-length
    // convention.
    pub fn idtx_raycast(
        params_ptr: *const u8,
        out_ptr: *mut u8,
        path_out_ptr: *mut u8,
        path_cap: u32,
    ) -> u32;
    pub fn idtx_arc_cast(
        params_ptr: *const u8,
        pts_out_ptr: *mut u8,
        pts_cap: u32,
        out_ptr: *mut u8,
        path_out_ptr: *mut u8,
        path_cap: u32,
    ) -> u32;
    pub fn idtx_find_by_type(type_hash: u64) -> u32;
    pub fn idtx_type_query_get(
        index: u32,
        out_entity_ptr: *mut u8,
        path_out_ptr: *mut u8,
        path_cap: u32,
    ) -> u32;

    // overlay / render commands (capability: overlay). Post a
    // transient highlight / material-override / post-process command for the
    // target prim; drained by the session into the render world each frame.
    pub fn idtx_overlay_command(
        kind: u32,
        target: u64,
        a_ptr: *const u8,
        a_len: u32,
        b_ptr: *const u8,
        b_len: u32,
        color_ptr: *const u8,
        scalar: f32,
    ) -> u32;

    // physics control (capability: physics) — grab suspend/resume. Set whether
    // the target body is simulated: `enabled == 0` suspends it (flip dynamic →
    // kinematic while grabbed), non-zero resumes dynamic simulation (release).
    // Deferred: the host enqueues it and the session applies it to its physics
    // subsystem before the physics tick. Absent without the `physics` grant.
    pub fn idtx_physics_set_simulated(target: u64, enabled: u32) -> u32;
    // Resolve a hit child collider up to its rigid-body root
    // (`UsdPhysicsRigidBodyAPI`); writes `Entity::NULL` when inside no rigid body.
    // Gated on the `physics` capability (feeds grab suspend/resume).
    pub fn idtx_scene_physics_body(entity: u64, out_ptr: *mut u8) -> u32;

    // level_control (capability: level_control) — request a runtime level
    // transition. Deferred: the host enqueues the request and the session
    // surfaces it for the runtime driver to perform the swap (never swaps inside
    // the import). `to_ptr`/`to_len` are the target level id; a zero-length
    // `spawn` (`spawn_len == 0`) means the target's default spawn, otherwise
    // `spawn_ptr`/`spawn_len` name a spawn point. Absent without the grant.
    pub fn idtx_request_level_transition(
        to_ptr: *const u8,
        to_len: u32,
        spawn_ptr: *const u8,
        spawn_len: u32,
    ) -> u32;
    pub fn idtx_state_set(key: *const u8, key_len: u32, value: *const u8, value_len: u32) -> u32;
    pub fn idtx_cancel_level_transition() -> u32;
    pub fn idtx_state_get(
        key: *const u8,
        key_len: u32,
        out: *mut u8,
        capacity: u32,
        length: *mut u32,
    ) -> u32;

    // input (capability: input)
    pub fn idtx_input_resolve(name_ptr: *const u8, name_len: u32) -> u32;
    pub fn idtx_input_action_phase(id: u32) -> u32;
    pub fn idtx_input_axis(id: u32) -> f32;
    pub fn idtx_input_axis2(id: u32, out_ptr: *mut u8) -> u32;
    pub fn idtx_input_mouse_delta(out_ptr: *mut u8);
    pub fn idtx_input_scroll() -> f32;

    // xr_hmd (capability: xr_hmd)
    pub fn idtx_xr_get_pose(space: u32, out_ptr: *mut u8) -> u32;
    // Play-space recenter offset (capability: xr_hmd). Writes an `abi::Vec3`
    // (12 bytes, stage-space ground translation, y = 0) the host re-centres the
    // pawn by; always Ok (zero when no recenter is active).
    pub fn idtx_xr_get_recenter(out_ptr: *mut u8) -> u32;
    // 26 × `abi::XrHandJointResult` (1040 bytes) for the given hand `side`
    // (`abi::XrSide` as u32). Returns `NotFound` when the hand is not tracked
    // this frame. (capability: xr_hmd).
    pub fn idtx_xr_get_hand_joints(side: u32, out_ptr: *mut u8) -> u32;
    // Which input source(s) drive `side` (`abi::XrSide` as u32) this frame,
    // returned directly as an `abi::XrInputSource` bitmask (CONTROLLER | HAND).
    // `0` (NONE) when the side is not tracked. (capability: xr_hmd).
    pub fn idtx_xr_get_tracking_mode(side: u32) -> u32;

    // events (capability: events) — read the event currently being
    // delivered to `on_event`. `_name`/`_payload` copy up to `cap` bytes
    // to `out_ptr` and return the event's *true* length.
    pub fn idtx_event_current_target() -> u64;
    pub fn idtx_event_current_name(out_ptr: *mut u8, cap: u32) -> u32;
    pub fn idtx_event_current_payload(out_ptr: *mut u8, cap: u32) -> u32;

    // widget interaction — ABI 3.
    pub fn idtx_widget_set_value(
        name: *const u8,
        name_len: u32,
        value: *const u8,
        value_len: u32,
    ) -> u32;
    pub fn idtx_widget_get_value(name: *const u8, name_len: u32, out: *mut u8, cap: u32) -> u32;
    // `register_handler` / `handle` require the `widget` capability.
    // `register_handler`: a panel behavior tells the host it handles `kind`
    // events on the panel-relative child `path` (NULL panel = the caller's own
    // panel entity), so the host can report `consumed` synchronously and only
    // dispatch events that have a handler.
    pub fn idtx_widget_register_handler(
        panel: u64,
        path_ptr: *const u8,
        path_len: u32,
        kind: u32,
    ) -> u32;
    // `handle`: a pointer behavior hands a widget raycast hit (`entity` + the
    // world-space hit `position`) to host-side widget processing. The host
    // recomputes the panel UV, draws the hit-point cue, and — when a `widgets.*`
    // action edge fired on an element with a registered handler — dispatches the
    // event this frame. Returns `1` when the widget consumed the interaction.
    pub fn idtx_widget_handle(entity: u64, px: f32, py: f32, pz: f32) -> u32;
    // Read the widget event currently being delivered to `_idtx_on_event`.
    // ALWAYS LINKED (no capability): the macro-generated `_idtx_on_event`
    // dispatcher in *every* behavior calls `_kind` to tell a widget event from a
    // regular one, so it must link regardless of grants. `_kind` returns the
    // `abi::WidgetEventKind` as u32, or `WIDGET_EVENT_NONE` when the current
    // event is not a widget event. `_element` copies up to `cap` bytes of the
    // panel-relative element path and returns its true length; `_uv` writes two
    // `f32` (u, v).
    pub fn idtx_widget_event_kind() -> u32;
    pub fn idtx_widget_event_element(out_ptr: *mut u8, cap: u32) -> u32;
    pub fn idtx_widget_event_uv(out_ptr: *mut u8);

    // auth (capability: auth_write) — write the process-global auth bucket the
    // OpenExec `auth:` provider resolves into an `Authorization` header. Write
    // only; there is no read-back import. Absent without the `auth_write` grant.
    pub fn idtx_auth_set_token(ptr: *const u8, len: u32) -> u32;
    pub fn idtx_auth_set_usercred(
        user_ptr: *const u8,
        user_len: u32,
        pass_ptr: *const u8,
        pass_len: u32,
    ) -> u32;
    pub fn idtx_auth_clear() -> u32;
    pub fn idtx_auth_revision() -> u64;
    // Host-scoped variants: bind a credential to a single host so a later `net`
    // request to THAT host (and only it) is authenticated — the credential is
    // never attached to a request to a different allowlisted host.
    pub fn idtx_auth_set_token_for_host(
        host_ptr: *const u8,
        host_len: u32,
        tok_ptr: *const u8,
        tok_len: u32,
    ) -> u32;
    pub fn idtx_auth_set_usercred_for_host(
        host_ptr: *const u8,
        host_len: u32,
        user_ptr: *const u8,
        user_len: u32,
        pass_ptr: *const u8,
        pass_len: u32,
    ) -> u32;

    // net (capability: net) — perform a blocking HTTP request host-side
    // and copy the response body back into a caller buffer. A general egress
    // primitive (method/url/content-type/body are all caller-supplied), absent
    // without the grant. `resp_out_ptr`/`resp_cap` receive the body (truncated
    // to `resp_cap`); `result_out_ptr` receives an `abi::HttpResult` (status +
    // the body's true length). Returns an `abi::ErrorCode` (`InvalidArgument`
    // on a transport-level failure; `Ok` even for a non-2xx HTTP status, which
    // the caller reads from `HttpResult::status`).
    pub fn idtx_http_request(
        method_ptr: *const u8,
        method_len: u32,
        url_ptr: *const u8,
        url_len: u32,
        ctype_ptr: *const u8,
        ctype_len: u32,
        body_ptr: *const u8,
        body_len: u32,
        resp_out_ptr: *mut u8,
        resp_cap: u32,
        result_out_ptr: *mut u8,
    ) -> u32;

    // net (capability: net) — perform an ASYNCHRONOUS HTTP request. Returns
    // immediately after validating + queueing; the host runs the request on a
    // background worker and later invokes the guest's `_idtx_on_http_response(
    // request_id, status)` export with the result. `request_id` is the guest's
    // own correlation token (the SDK routes it to the registered handler). The
    // return `abi::ErrorCode` reports only up-front rejection (malformed method,
    // too many in-flight); transport failure / non-2xx surface via the callback.
    pub fn idtx_http_request_async(
        request_id: u32,
        method_ptr: *const u8,
        method_len: u32,
        url_ptr: *const u8,
        url_len: u32,
        ctype_ptr: *const u8,
        ctype_len: u32,
        body_ptr: *const u8,
        body_len: u32,
    ) -> u32;
    // Read the body of the async HTTP response currently being delivered to
    // `_idtx_on_http_response` (the status is passed to that export directly).
    // Copies up to `cap` bytes to `out_ptr` and returns the body's *true* length.
    pub fn idtx_http_response_body(out_ptr: *mut u8, cap: u32) -> u32;
}

// ---------------------------------------------------------------------------
// Behavior singleton
// ---------------------------------------------------------------------------
//
// Each guest module hosts exactly one behavior instance. The host instantiates
// the module once (one wasmtime `Store` + linear memory per attachment) and
// dispatches every lifecycle hook against it, so the module's linear memory —
// and therefore the behavior instance stored in [`BehaviorSlot`] below —
// persists across hooks and frames. The instance is lazily constructed on the
// first hook and lives until the host calls the generated `_idtx_drop_instance`
// export after the terminal lifecycle hook (`on_detach` / `on_destroy`), i.e.
// when the owning prim is removed/deactivated or the app tears down.

/// A single-threaded, lazily-initialised slot holding the module's one behavior
/// instance across hook invocations.
///
/// A WASM guest module hosts exactly one behavior and the host dispatches its
/// hooks sequentially on a single thread, never re-entering the guest while a
/// hook runs (host imports are pure data marshalling and session edits are
/// deferred — no import calls back into a guest export mid-hook). The instance
/// is stored in a [`RefCell`](core::cell::RefCell) so that borrow uniqueness is
/// *checked at run time* rather than merely assumed: if a hook ever did re-enter
/// [`with`](Self::with) on the same slot (e.g. author code calls one of the raw
/// exported thunks from inside another hook), the nested `borrow_mut` panics —
/// which traps the guest and the host disables the instance — instead of
/// producing a second aliasing `&mut T` and undefined behaviour.
///
/// The [`Sync`] impl below is sound only under the single-threaded WASM
/// guarantee, so it is restricted to non-threaded `wasm32` (`wasm32` *without*
/// the `atomics` target feature — a guest built with atomics/shared-memory can
/// spawn threads, where the guarantee no longer holds). `RefCell` is `!Sync`, so
/// on any other target — native, or a threaded wasm guest — a consumer that
/// shared the slot across threads is rejected by the type system (a threaded
/// guest fails to compile the generated `static` rather than silently relying on
/// an invariant that does not hold) instead of hitting undefined behaviour.
#[doc(hidden)]
pub struct BehaviorSlot<T> {
    cell: core::cell::RefCell<Option<T>>,
}

// SAFETY: a non-threaded WASM guest is single-threaded, so no concurrent access
// to `cell` is possible. The impl exists only so the slot can back a `static`
// (which Rust requires to be `Sync`), and that static is itself generated only
// on `wasm32` (see the macro crate). It is withheld on native targets and on
// threaded wasm (`target_feature = "atomics"`), where the single-threaded
// invariant does not hold — there `RefCell`'s own `!Sync` keeps the type honest
// (a threaded guest then fails to compile the generated static, by design).
#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
unsafe impl<T> Sync for BehaviorSlot<T> {}

impl<T> BehaviorSlot<T> {
    /// A fresh, empty slot (`const` so it can initialise a `static`).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cell: core::cell::RefCell::new(None),
        }
    }

    /// Run `f` against the stored instance, lazily constructing it with `init`
    /// on first use. The instance persists after `f` returns, so author state
    /// written through `&mut self` survives into the next hook.
    ///
    /// # Panics
    ///
    /// Panics if called re-entrantly on the same slot (a nested `with` while an
    /// outer `with` is still running). On the guest this is a trap, which the
    /// host treats as a fatal fault and disables the instance — a deterministic
    /// failure in place of the undefined behaviour a second aliasing `&mut T`
    /// would cause.
    pub fn with<R>(&self, init: impl FnOnce() -> T, f: impl FnOnce(&mut T) -> R) -> R {
        let mut slot = self.cell.borrow_mut();
        let inst = slot.get_or_insert_with(init);
        f(inst)
    }

    /// Drop the stored instance (if any), running its destructor. A subsequent
    /// [`with`](Self::with) re-constructs a fresh default. Invoked by the
    /// generated `_idtx_drop_instance` export after the terminal lifecycle hook.
    pub fn clear(&self) {
        *self.cell.borrow_mut() = None;
    }
}

impl<T> Default for BehaviorSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Install the guest panic hook (traps on panic so the host never sees a
/// partially-mutated store). No-op natively.
pub fn install_panic_hook() {
    // Nothing to install natively; on wasm the default abort behavior
    // already traps. Real hook wiring lands with richer diagnostics.
}

#[cfg(test)]
mod tests {
    use super::BehaviorSlot;
    use core::cell::Cell;

    #[derive(Default)]
    struct Counter {
        hits: u32,
    }

    #[test]
    fn with_persists_state_and_constructs_once() {
        let slot = BehaviorSlot::<Counter>::new();
        let mut inits = 0u32;
        for _ in 0..3 {
            slot.with(
                || {
                    inits += 1;
                    Counter::default()
                },
                |c| c.hits += 1,
            );
        }
        // Constructed exactly once; state accumulated across the three hooks.
        assert_eq!(inits, 1, "init ran once, not per hook");
        slot.with(Counter::default, |c| assert_eq!(c.hits, 3));
    }

    #[test]
    fn clear_drops_instance_and_resets() {
        // A type whose Drop flips a flag, so we can observe the slot really
        // dropping the stored instance (not just forgetting it).
        struct Guard<'a>(&'a Cell<u32>);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }

        let drops = Cell::new(0u32);
        let slot = BehaviorSlot::<Guard<'_>>::new();
        slot.with(|| Guard(&drops), |_| {});
        assert_eq!(drops.get(), 0, "instance still alive before clear");

        slot.clear();
        assert_eq!(drops.get(), 1, "clear ran the instance's destructor");

        // A hook after clear re-constructs a fresh instance.
        slot.with(|| Guard(&drops), |_| {});
        slot.clear();
        assert_eq!(drops.get(), 2, "fresh instance constructed then dropped");
    }

    #[test]
    #[should_panic(expected = "already borrowed")]
    fn reentrant_with_traps_instead_of_aliasing() {
        // A re-entrant `with` on the same slot (e.g. author code invoking a raw
        // exported thunk from inside another hook) must trap via the `RefCell`
        // borrow check rather than hand out a second aliasing `&mut T`.
        let slot = BehaviorSlot::<Counter>::new();
        slot.with(Counter::default, |_outer| {
            slot.with(Counter::default, |_inner| {});
        });
    }
}

// ---------------------------------------------------------------------------
// Dispatch entry points invoked by the generated thunks
// ---------------------------------------------------------------------------

fn report_hook_result(result: Result<()>) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        idtx_hook_result(result.map_or_else(|error| error.code().as_u32(), |()| 0));
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = result;
}

/// Dispatch a no-argument hook. Builds a `Ctx`, runs `f`, swallows the
/// `Result` (logging on wasm). The generated thunk supplies `f`.
pub fn dispatch<F>(f: F)
where
    F: FnOnce(&Ctx) -> Result<()>,
{
    let ctx = Ctx::current();
    report_hook_result(f(&ctx));
}

/// Dispatch a `dt`-carrying hook (`on_update` family). The `dt` is already
/// captured by the closure the thunk builds; this mirrors [`dispatch`] so
/// the two thunk shapes have a symmetric entry point.
pub fn dispatch_dt<F>(f: F)
where
    F: FnOnce(&Ctx) -> Result<()>,
{
    let ctx = Ctx::current();
    report_hook_result(f(&ctx));
}

/// Dispatch a stage-level lifecycle event hook (`on_stage_event`). The raw
/// `u32` wire code is decoded to a [`crate::abi::StageEvent`] before the
/// author closure runs. Mirrors [`dispatch`]/[`dispatch_dt`].
pub fn dispatch_u32<F>(code: u32, f: F)
where
    F: FnOnce(&Ctx, crate::abi::StageEvent) -> Result<()>,
{
    let ctx = Ctx::current();
    let event = crate::abi::StageEvent::from_u32(code);
    report_hook_result(f(&ctx, event));
}

/// Dispatch an input edge hook (`on_input`). The raw wire values are
/// decoded to an [`crate::abi::InputActionId`] + [`crate::abi::InputPhase`]
/// before the author closure runs. Mirrors [`dispatch_u32`].
pub fn dispatch_input<F>(action_id: u32, phase: u32, f: F)
where
    F: FnOnce(&Ctx, crate::abi::InputActionId, crate::abi::InputPhase) -> Result<()>,
{
    let ctx = Ctx::current();
    let action = crate::abi::InputActionId(action_id);
    let phase = crate::abi::InputPhase::from_u32(phase);
    report_hook_result(f(&ctx, action, phase));
}
