//! UI-neutral level-transition driver core (level-transition P5a).
//!
//! The keystone of the per-level-assembly level transition: a tiny,
//! non-blocking state machine that services a [`LevelTransitionRequest`] a
//! `level_control` behavior posted, cuts to the resident transfer/mask scene,
//! loads the target level's assembly on a worker thread (via [`StageLoader`]),
//! and swaps it in once ready — placing the player at the target spawn.
//!
//! This is the loop-agnostic core. The windowed player wires the same driver
//! into its own frame loop (a separately reviewed commit); the headless loop in
//! `idtx_runtime_core::run_headless` wires it here. All it needs each frame is one
//! [`TransitionDriver::poll`] call right after `tick_behaviors` and before the
//! render, handed the [`SessionSet`] it advances.
//!
//! State machine (`docs/level-transition-runtime-plan.md` §2):
//!
//! ```text
//! Idle:
//!   drain the active level's pending request.
//!   on request(target, spawn):
//!     resolve target id → assembly path (LevelTable)
//!       resolves   → show_transfer() (cut to the mask), StageLoader::request(..),
//!                    phase = Loading { loader, spawn }
//!       no resolve → warn, drop the request, stay Idle
//! Loading:
//!   poll the loader:
//!     Pending   → keep masking
//!     Ok(new)   → place the player (placement precedence: a one-shot
//!                 GlobalState override wins, else the target's /World/level/Spawn);
//!                 set_current(new) → returns the outgoing session (caller tears
//!                 its behaviors down against its own world, then drops it); Idle
//!     Err(e)    → warn; show_current() (abort back to the still-resident level); Idle
//! ```
//!
//! **Carried state across a swap.** The driver owns a [`GlobalState`] — the
//! one object that spans a level swap, so experience state placed on it (a small
//! key/value blob + a one-shot player placement override) survives while sessions
//! are dropped and replaced. **Behavior `Store` (wasmtime) state does NOT survive
//! a transition** — dropping a session drops its `BehaviorHost`; a behavior that
//! must persist state or decide *when* to transition belongs on the shared player
//! sublayer and reads/writes bounded experience state through the guest SDK. The
//! placement override is consumed once at the next cutover; absent it, the target
//! level's `/World/level/Spawn` is used. See
//! `docs/level-transition-runtime-plan.md` §2 and [`crate::global_state`].
//!
//! The outgoing level stays **resident** but un-ticked/un-rendered (active =
//! `Transfer`) for the duration of the load; it is dropped only at cutover via
//! [`SessionSet::set_current`]. So peak memory during a swap is
//! `old + transfer + new` — a deliberate 3-session peak (not the lighter
//! `transfer + new`): keeping the old level resident buys an instant,
//! reload-free abort on a load failure ([`SessionSet`]'s `current` is not an
//! `Option`, so it cannot simply be dropped early). A memory-tight target
//! (Quest / Vision Pro) could later make `current` optional and drop it at the
//! cut, recording a `prev_level` path to reload on failure; see
//! `docs/level-transition-runtime-plan.md` §2/§7.
//!
//! Only one transition runs at a time: while `Loading`, [`poll`] services the
//! loader and does not start another load. A bounded cancellation flag from the
//! active session returns to the retained level and cancels publication.
//!
//! [`poll`]: TransitionDriver::poll

use std::path::{Path, PathBuf};
use std::task::Poll;
use std::sync::Arc;
use std::time::Duration;

use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

use crate::global_state::GlobalState;
use crate::session_set::SessionSet;
use crate::stage_loader::{StageLoader, LoadPreparation, LoadPhase, StageLoadError, PrepareSession};

/// Configures a freshly-loaded bare [`StudioSession`] on its loading worker — the
/// per-session setup (behavior host, input map, scene camera, network policy)
/// the boot/transfer session also runs. Supplied by the embedding runtime so
/// the driver stays independent of its `RunConfig`: the desktop/Quest runtime
/// wraps `configure_runtime_session`, the visionOS runtime wires its own. The
/// driver additionally forces editor overlays off and corrects the viewport
/// before invoking this (see [`TransitionDriver::poll`]).
pub type SessionConfigure = Box<dyn Fn(&mut StudioSession) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionStatus { Idle, Loading(LoadPhase), Completed, Cancelled, TimedOut, Failed(String) }

#[cfg(feature = "metrics")]
#[derive(Debug, Clone, Copy)]
pub struct TransitionMetrics {
    pub load_us: u64,
    pub cutover_us: u64,
    pub live_stages: u64,
    pub peak_rss_bytes: u64,
}

/// Maps a behavior-named level **id** to its per-level assembly path.
///
/// A `level_control` behavior names a target by its level *id* (e.g. `"cave"`);
/// the runtime resolves it to the cooked assembly path. The id of an assembly
/// path `<bundle>/levels/<id>/level.usda` is the **parent directory name**
/// (`<id>`) — the stable holder the cook emits — matched **case-sensitively**.
pub struct LevelTable {
    levels: Vec<PathBuf>,
}

impl LevelTable {
    /// Build a table from an explicit list of per-level assembly paths
    /// (absolute entries). Empty for a single-stage experience (no transitions
    /// possible). The embedding runtime passes `cfg.levels` here.
    #[must_use]
    pub fn new(levels: Vec<PathBuf>) -> Self {
        Self { levels }
    }

    /// Resolve a level `id` (the parent directory name of a `level.usda`) to its
    /// assembly path, case-sensitively. `None` when no registered level matches.
    #[must_use]
    pub fn resolve(&self, id: &str) -> Option<&Path> {
        self.levels
            .iter()
            .find(|p| level_id_of(p.as_path()) == Some(id))
            .map(PathBuf::as_path)
    }

    /// `true` when no levels are registered (a single-stage experience).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    /// Number of registered levels.
    #[must_use]
    pub fn len(&self) -> usize {
        self.levels.len()
    }
}

/// The level id of an assembly path: the name of the directory that **contains**
/// the assembly file (`<bundle>/levels/<id>/level.usda` → `"<id>"`). `None` when
/// the path has no parent directory or the name is not valid UTF-8.
fn level_id_of(path: &Path) -> Option<&str> {
    path.parent()?.file_name()?.to_str()
}

/// The driver's state: idle, or a load of the next level in flight.
enum Phase {
    /// No transition is running — [`TransitionDriver::poll`] drains the active
    /// level's pending request each call.
    Idle,
    /// A target level is loading on a worker thread; the transfer/mask scene is
    /// masking. `spawn` is the requested spawn point to place the player at once
    /// the load completes.
    Loading {
        loader: StageLoader,
        spawn: Option<String>,
    },
}

/// Drives level transitions one non-blocking step per [`poll`](Self::poll).
///
/// Holds the render parameters needed to spawn a [`StageLoader`] (cloned `wgpu`
/// device/queue are cheap `Arc` handles), the [`LevelTable`] that resolves a
/// behavior-named target id to an assembly path, and the current [`Phase`].
///
/// Built only for a multi-level experience (`cfg.levels` non-empty); a
/// single-stage run constructs no driver and the frame loop never polls one.
pub struct TransitionDriver {
    device: wgpu::Device,
    queue: wgpu::Queue,
    viewport: [u32; 2],
    texture_config: TextureDecodeConfig,
    world_unit_scale: RenderWorldUnitScale,
    levels: LevelTable,
    /// Configures an incoming level before handoff. A freshly loaded
    /// [`StageLoader`] session is a **bare** [`StudioSession`] (no behavior host
    /// wiring, no input map, no scene camera); without running the same
    /// per-session setup the initial boot applies, the new level renders its
    /// geometry through the fallback orbit camera and attaches **no** authored
    /// behaviors — so player input/movement is dead after the swap. The
    /// embedding runtime supplies this closure (see [`SessionConfigure`]), which
    /// keeps the driver independent of `RunConfig` so the winit-free visionOS
    /// runtime can reuse it.
    configure: PrepareSession,
    prepare: Option<PrepareSession>,
    timeout: Duration,
    status: TransitionStatus,
    resident_state_bound: bool,
    #[cfg(feature = "metrics")]
    started: Option<std::time::Instant>,
    #[cfg(feature = "metrics")]
    metrics: Option<TransitionMetrics>,
    /// The anonymous session-layer USDA composed onto every swapped-in level at
    /// open (the same layer the boot/transfer sessions receive). Empty `""` for
    /// none. This is how the runtime-resolved XR controller device `Model`
    /// injection (`controller_model_over_layer`) reaches a level loaded by a
    /// transition: the packaged rig carries only bare controller *pose* Xforms,
    /// so without re-applying this `over` the swapped-in level's controllers
    /// hydrate as geometry-less prims and render nothing. The boot path injects
    /// it into the initial level and the transfer/mask session; this field
    /// carries it to each subsequently loaded level.
    session_layer: String,
    phase: Phase,
    /// Experience state carried across level swaps. Owned here — not on a
    /// session — precisely because the driver is the one object that spans a
    /// swap: it persists for the whole run while [`SessionSet::set_current`]
    /// drops and replaces sessions. The spec
    /// (`docs/level-transition-runtime-plan.md` §2) names `RuntimePlayer` as the
    /// owner, but the driver is the loop-agnostic equivalent both the headless
    /// (`idtx_runtime_core::run_headless`) and windowed (`player.rs`) loops already hold,
    /// so the carried blob + placement override survive a `set_current` for free.
    global: GlobalState,
}

impl TransitionDriver {
    /// Build a driver over a resolved [`LevelTable`], the render parameters a
    /// load is spawned with (full-resolution textures, metre world units), and
    /// the per-session `configure` closure the embedding runtime supplies for
    /// cutover. `session_layer` is the anonymous session-layer USDA applied to
    /// every swapped-in level (the XR controller-device `Model` injection the
    /// boot/transfer sessions also receive; `""` for none). Desktop/Quest callers
    /// use the `RunConfig` wrapper `idtx_runtime_core::build_transition_driver`;
    /// visionOS wires its own.
    #[must_use]
    pub fn new(
        levels: LevelTable,
        device: wgpu::Device,
        queue: wgpu::Queue,
        viewport: [u32; 2],
        configure: SessionConfigure,
        session_layer: String,
    ) -> Self {
        Self {
            device,
            queue,
            viewport,
            texture_config: TextureDecodeConfig::FULL_RESOLUTION,
            world_unit_scale: RenderWorldUnitScale::METRE,
            levels,
            configure: Arc::from(configure),
            prepare: None,
            timeout: Duration::from_secs(30),
            status: TransitionStatus::Idle,
            resident_state_bound: false,
            #[cfg(feature = "metrics")]
            started: None,
            #[cfg(feature = "metrics")]
            metrics: None,
            session_layer,
            phase: Phase::Idle,
            global: GlobalState::default(),
        }
    }

    /// `true` while a transition is in flight (a level is loading). The frame
    /// loop renders the transfer/mask scene until this returns `false`.
    #[must_use]
    pub fn is_transitioning(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    #[must_use]
    pub fn with_state(mut self, state: idtx_behavior::ExperienceState) -> Self { self.global.bind(state); self }
    pub fn set_prepare(&mut self, prepare: PrepareSession) { self.prepare = Some(prepare); }
    pub fn set_timeout(&mut self, timeout: Duration) { self.timeout = timeout; }
    #[must_use]
    pub fn status(&self) -> TransitionStatus {
        if let Phase::Loading { loader, .. } = &self.phase { TransitionStatus::Loading(loader.phase()) } else { self.status.clone() }
    }
    pub fn cancel(&mut self, sessions: &mut SessionSet) -> bool {
        if !self.is_transitioning() { return false; }
        if let Phase::Loading { loader, .. } = std::mem::replace(&mut self.phase, Phase::Idle) { loader.cancel(); }
        sessions.show_current();
        self.status = TransitionStatus::Cancelled;
        true
    }
    #[cfg(feature = "metrics")]
    #[must_use]
    pub fn metrics(&self) -> Option<TransitionMetrics> { self.metrics }

    /// The swap-spanning experience state ([`GlobalState`]). Carried on the driver
    /// so it outlives every session drop (see the field doc / §2 ownership note).
    #[must_use]
    pub fn global_state(&self) -> &GlobalState {
        &self.global
    }

    /// Mutable access to the swap-spanning [`GlobalState`] — set a blob value or a
    /// one-shot player placement override before firing a transition.
    pub fn global_state_mut(&mut self) -> &mut GlobalState {
        &mut self.global
    }

    /// The level table this driver resolves targets against.
    #[must_use]
    pub fn levels(&self) -> &LevelTable {
        &self.levels
    }

    /// Update the viewport size future loads open their session at — call on a
    /// window resize so a level loaded after the resize matches the window.
    pub fn set_viewport(&mut self, viewport: [u32; 2]) {
        self.viewport = viewport;
    }

    /// Advance the state machine exactly one step. Cheap and non-blocking: it
    /// never waits on the loader, so it is safe to call every frame.
    ///
    /// - **Idle:** drains the current level's pending [`LevelTransitionRequest`].
    ///   A resolvable target cuts to the transfer scene and starts an async load;
    ///   an unresolvable target is warned about and dropped (stays idle). No
    ///   request → nothing happens.
    /// - **Loading:** polls the loader. Still pending → keep masking. Ready → the
    ///   player is placed (a one-shot [`GlobalState`] placement override wins over
    ///   the target level's spawn), the new session is swapped in (the old
    ///   current is dropped), and the driver returns to idle. Load error →
    ///   warned, the set aborts back to the still-resident current level, idle.
    pub fn poll(&mut self, sessions: &mut SessionSet) -> Option<StudioSession> {
        if sessions.active_session_mut().take_pending_level_cancellation() {
            sessions.current_mut().take_pending_level_transition();
            if let Some(mask) = sessions.transfer_mut() { mask.take_pending_level_transition(); }
            self.cancel(sessions);
            return None;
        }
        if !self.resident_state_bound {
            self.texture_config = sessions.current().texture_config();
            sessions.current_mut().set_experience_state(self.global.shared());
            if let Some(transfer) = sessions.transfer_mut() { transfer.set_experience_state(self.global.shared()); }
            self.resident_state_bound = true;
        }
        #[cfg(feature = "metrics")]
        let frame_started = std::time::Instant::now();
        // Take the phase by value so the borrow checker sees disjoint access to
        // the driver's other fields (device/queue/levels) inside each arm; a
        // `Pending` load restores `Loading` before returning.
        //
        // Returns the outgoing level session **only** at cutover (the
        // `Poll::Ready(Ok)` arm), so the caller can tear its behaviors down
        // against that session's own world before dropping it (see
        // [`SessionSet::set_current`]). Every other path returns `None`.
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Idle => {
                let req = sessions.current_mut().take_pending_level_transition()?;
                let Some(path) = self.levels.resolve(&req.target).map(Path::to_path_buf) else {
                    self.status = TransitionStatus::Failed("unknown level".into());
                    tracing::warn!(
                        target = %req.target,
                        "level_control transition target does not resolve to a known level; \
                         dropping the request"
                    );
                    return None;
                };
                // Cut to the mask instantly (no-op if no transfer session exists),
                // then start the async load of the target assembly on a worker.
                let Ok(state) = self.global.shared().fork() else {
                    self.status = TransitionStatus::Failed("experience state unavailable".into()); return None;
                };
                sessions.show_transfer();
                let loader = StageLoader::request_prepared(
                    &path,
                    self.device.clone(),
                    self.queue.clone(),
                    self.viewport,
                    self.texture_config,
                    self.world_unit_scale,
                    &self.session_layer,
                    LoadPreparation { configure: Some(self.configure.clone()), prepare: self.prepare.clone(), state: Some(state), timeout: self.timeout },
                );
                #[cfg(feature = "metrics")]
                { self.started = Some(std::time::Instant::now()); }
                tracing::info!(
                    target = %req.target,
                    level = %path.display(),
                    "level transition started; masking with the transfer scene while loading"
                );
                self.phase = Phase::Loading {
                    loader,
                    spawn: req.spawn,
                };
                None
            }
            Phase::Loading { loader, spawn } => match loader.try_recv() {
                Poll::Pending => {
                    // Still loading — keep the transfer scene masking.
                    self.phase = Phase::Loading { loader, spawn };
                    None
                }
                Poll::Ready(Ok(mut new)) => {
                    // A `StageLoader` session is bare: match the boot + transfer
                    // setup before swapping it in.
                    //
                    // 1. Reassert runtime overlay suppression at cutover. The
                    //    shared runtime configuration already disables camera
                    //    frusta and other editor overlays before worker warm-up,
                    //    including builds with Cargo-unified studio features.
                    // 2. Correct the viewport: the in-flight loader captured the
                    //    window size at transition *start*; a resize during the
                    //    async load only updated `self.viewport` (future loads) via
                    //    `set_viewport`, leaving this session's frame target + camera
                    //    aspect stale. `self.viewport` holds the live surface size.
                    new.set_editor_overlays_enabled(false);
                    new.resize_viewport(self.viewport);
                    // Configuration, WASM attachment and resource preparation
                    // completed on the worker. Publish its isolated initialization
                    // state only if the running experience has not conflicted.
                    if new.experience_state().commit_into(&self.global.shared()).is_err() {
                        self.status = TransitionStatus::Failed("experience state changed during preparation".into());
                        sessions.show_current();
                        return None;
                    }
                    new.set_experience_state(self.global.shared());

                    // Placement precedence: a one-shot `GlobalState` player
                    // placement override beats the target level's spawn. If an
                    // override is carried, apply it and consume it (so it never
                    // leaks into a later transition); otherwise fall back to the
                    // target level's `/World/level/Spawn` (the default).
                    let placed;
                    if let Some(world) = self.global.player_placement() {
                        placed = new.place_player_at_transform(world);
                        if placed { self.global.clear_player_placement(); }
                        tracing::info!(
                            placed,
                            "level loaded; applied the one-shot GlobalState player placement \
                             override (cleared) and swapped in the new level, dropping the old one"
                        );
                    } else {
                        placed = new.place_player_at_spawn(spawn.as_deref());
                        tracing::info!(
                            spawn = ?spawn,
                            placed,
                            "level loaded; placed the player at the target level spawn and \
                             swapped in the new level, dropping the old one"
                        );
                    }
                    // Cutover: swaps in the new level and returns the outgoing
                    // one so the caller can tear its behaviors down against its
                    // own world before dropping it.
                    let outgoing = sessions.set_current(new);
                    self.status = TransitionStatus::Completed;
                    #[cfg(feature = "metrics")]
                    {
                        let metrics = TransitionMetrics {
                            load_us: self.started.take().map_or(0, |started| u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)),
                            cutover_us: u64::try_from(frame_started.elapsed().as_micros()).unwrap_or(u64::MAX),
                            live_stages: idtx_usd::live_stage_handle_count(),
                            peak_rss_bytes: crate::runtime::peak_rss_bytes(),
                        };
                        tracing::info!(?metrics, "level cutover");
                        self.metrics = Some(metrics);
                    }
                    // phase already Idle (from the mem::replace above).
                    Some(outgoing)
                }
                Poll::Ready(Err(e)) => {
                    self.status = match &e {
                        StageLoadError::Cancelled => TransitionStatus::Cancelled,
                        StageLoadError::TimedOut => TransitionStatus::TimedOut,
                        _ => TransitionStatus::Failed(e.to_string()),
                    };
                    tracing::warn!(
                        error = %e,
                        "level load failed; aborting the transition back to the current level"
                    );
                    // Abort: the outgoing level is still resident — cut back to it.
                    // A pending one-shot GlobalState player-placement override is
                    // intentionally NOT cleared here: it was never consumed (no new
                    // level was placed), so it still applies to the next attempt.
                    sessions.show_current();
                    // phase already Idle.
                    None
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> LevelTable {
        LevelTable::new(vec![
            PathBuf::from("/bundle/levels/forest/level.usda"),
            PathBuf::from("/bundle/levels/cave/level.usda"),
        ])
    }

    #[test]
    fn resolves_level_id_by_parent_directory_name() {
        let t = table();
        assert_eq!(t.len(), 2);
        assert!(!t.is_empty());
        assert_eq!(
            t.resolve("forest"),
            Some(Path::new("/bundle/levels/forest/level.usda"))
        );
        assert_eq!(
            t.resolve("cave"),
            Some(Path::new("/bundle/levels/cave/level.usda"))
        );
    }

    #[test]
    fn unknown_level_id_does_not_resolve() {
        assert!(table().resolve("summit").is_none());
    }

    #[test]
    fn level_id_resolution_is_case_sensitive() {
        // "forest" is registered; "Forest" must not match.
        assert!(table().resolve("Forest").is_none());
    }

    #[test]
    fn empty_table_resolves_nothing() {
        let t = LevelTable::new(Vec::new());
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
        assert!(t.resolve("forest").is_none());
    }
}
