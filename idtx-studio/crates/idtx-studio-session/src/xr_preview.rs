//! `OpenXR` preview session (Phases 2.4, 3.3, 4.1).
//!
//! [`XrPreviewSession`] owns the XR session lifetime for a single "Play in
//! `OpenXR`" preview: the [`XrPlatform`] (lifecycle + HMD / controller poses)
//! and the fallback controller proxy entities. It is created against the
//! session's live ECS `World` and stepped once per frame; when no XR device
//! is present it is never created and the session behaves exactly as before.
//!
//! # Camera ownership
//!
//! The preview does **not** own a camera. The HMD-sync behavior is authored
//! on `/World/Player/Camera` (a hydrated prim of the XR player rig) and
//! attached via the session's authored-behavior path; it drives that prim's
//! `WorldTransform` through the session layer. The desktop mirror follows the
//! headset through the normal scene-camera path
//! ([`StudioSession::use_scene_camera`](crate::StudioSession::use_scene_camera)
//! + `scene_camera_desc`), and real stereo submission builds its eye views
//! directly from the located `OpenXR` eye poses (`xr_render::eye_camera`).
//!
//! # Frame flow (per [`Self::feed_snapshot`] + host `run_frame` + [`Self::sweep`])
//!
//! ```text
//! poll HMD/controllers ─▶ feed_xr_snapshot ─▶ host.run_frame(Playing)
//!                                                │  (authored sync behaviors
//!                                                │   author prim WorldTransforms)
//!                                                ▼
//!                                          controller sweep ─▶ GlobalTransform
//! ```

use glam::Mat4;
use idtx_scene::bevy_ecs::prelude::*;

use idtx_behavior::abi::{
    XR_HAND_JOINT_COUNT, Xform, XrHandJointResult, XrInputSource, XrPoseResult, XrSide, XrSpace,
};
use idtx_behavior::ecs as bec;
use idtx_behavior::{BehaviorHost, ModuleError, XrSnapshot};
use idtx_openxr_sys::{XrError, XrLoader};
use idtx_platform_xr::{HmdPose, XrPlatform, XrStateError};

#[cfg(feature = "gesture-recognition")]
use idtx_platform_xr::{GestureConfig, GestureEvent, GestureLink, GesturePhase, HandSnapshot};

/// Outcome of beginning a stereo XR frame (Stage C, real path).
#[cfg(idtx_xr)]
#[derive(Debug)]
pub enum XrBeginOutcome {
    /// Render both eyes described by this frame, then call `submit_xr_frame`.
    Frame(idtx_openxr_sys::XrFrame),
    /// Session is running but there is nothing to render this tick.
    Skip,
    /// The runtime ended the session (or it was lost); stop the preview. Holds
    /// an optional error message (`None` for a clean exit).
    Exit(Option<String>),
}

/// Failure surface for starting / driving an XR preview.
#[derive(Debug, thiserror::Error)]
pub enum XrPreviewError {
    /// No XR runtime / device was detected.
    #[error("no OpenXR device available")]
    NoDevice,
    /// The XR loader failed while probing.
    #[error("OpenXR loader error: {0}")]
    Loader(#[from] XrError),
    /// Registering or attaching the camera behavior failed.
    #[error("behavior error: {0}")]
    Behavior(#[from] ModuleError),
    /// An invalid XR lifecycle transition was requested.
    #[error("XR lifecycle error: {0}")]
    Lifecycle(#[from] XrStateError),
}

/// A bridge-only fallback ECS entity representing one XR controller.
///
/// The entity carries [`bec::Transform`], [`bec::GlobalTransform`], and
/// [`bec::UsdPath`]. It runs no behavior — its `GlobalTransform` is seeded
/// directly from the platform grip pose each frame (see
/// [`XrPreviewSession::sweep`]). It exists only for a hand whose real prim
/// did not author a `*_controller_sync` behavior; the world matrix is
/// consumed by the render injection.
#[derive(Debug)]
pub struct ControllerEntity {
    /// The ECS entity.
    pub entity: Entity,
    /// Stable USD path used as a render cache key.
    pub path: String,
}

/// Owns one "Play in `OpenXR`" preview: the XR lifetime and the fallback
/// controller proxy entities. The behavior host is **not** owned here — the
/// session owns a single [`BehaviorHost`] and passes it into
/// [`Self::feed_snapshot`]; the authored sync behaviors (camera + controllers)
/// are attached onto it by the session's authored-behavior path, and this
/// session only feeds the XR snapshot and sweeps the fallback proxy transforms.
#[derive(Debug)]
pub struct XrPreviewSession {
    platform: XrPlatform,
    /// Bridge-only fallback entity for the left controller, spawned only when
    /// the stage did not author a `left_controller_sync` behavior.
    left_controller: Option<ControllerEntity>,
    /// Bridge-only fallback entity for the right controller.
    right_controller: Option<ControllerEntity>,
    /// `true` once the left controller has received at least one tracked pose.
    /// Guards [`Self::controller_world_matrix`] from returning `Some(IDENTITY)`
    /// before any real pose has arrived (which would render the capsule at the
    /// world origin, outside the camera frustum).
    has_left_pose: bool,
    /// `true` once the right controller has received at least one tracked pose.
    has_right_pose: bool,
    /// The real graphics-bound `OpenXR` session. `None` in the
    /// mock / desktop path; `Some` once the shell attaches a D3D12 session.
    #[cfg(idtx_xr)]
    xr_session: Option<idtx_openxr_sys::XrGraphicsSession>,
    /// Background hand-gesture recogniser + its most recent output (Phase 10).
    /// `None` until [`Self::enable_gesture_recognition`] spawns the worker.
    #[cfg(feature = "gesture-recognition")]
    gesture: Option<GestureRuntime>,
}

/// Per-preview gesture recognition state.
#[cfg(feature = "gesture-recognition")]
#[derive(Debug)]
struct GestureRuntime {
    /// Frame-loop handle to the recognition thread.
    link: GestureLink,
    /// Gesture source strings that were active last frame (for edge derivation
    /// by the [`InputRouter`]).
    prev_sources: std::collections::HashSet<String>,
    /// Monotonic frame counter fed to the worker (diagnostics).
    frame: u64,
    /// The events drained on the most recent frame (for direct consumers such
    /// as the shell, in addition to the behavior input router).
    last_events: Vec<GestureEvent>,
}

impl XrPreviewSession {
    /// Probe for an XR device, spawn the fallback controller proxies, and
    /// drive the lifecycle to `Focused`.
    ///
    /// The camera is not resolved or attached here — the HMD-sync behavior is
    /// authored on `/World/Player/Camera` and attached by the session's
    /// authored-behavior path. `controller_behavior_owned[i]` is `true` when
    /// the stage already authored a `*_controller_sync` behavior on the real
    /// controller prim; that hand then has no fallback proxy (single authority).
    ///
    /// # Errors
    ///
    /// [`XrPreviewError::NoDevice`] when no runtime is present, or
    /// [`XrPreviewError::Lifecycle`] on an invalid transition.
    pub fn start(
        loader: &XrLoader,
        world: &mut World,
        controller_behavior_owned: [bool; 2],
    ) -> Result<Self, XrPreviewError> {
        // Device detection: refuse to start without a runtime.
        //
        // On the real D3D12 path the shell already holds a *persistent*
        // `XrInstance` (proof a device exists), and the Oculus runtime allows
        // only ONE instance per process — calling `available_runtimes()` here
        // would create a second, transient instance and fail with
        // `ERROR_LIMIT_REACHED`. So the check runs only off the real path.
        #[cfg(not(idtx_xr))]
        if loader.available_runtimes()?.is_empty() {
            return Err(XrPreviewError::NoDevice);
        }
        #[cfg(idtx_xr)]
        let _ = loader;

        // Spawn bridge-only fallback ECS entities for the left and
        // right controllers. These carry only pose transforms (seeded directly
        // from the platform in `sweep_or_seed_controller` path 2) and run no
        // behavior; they exist so the render path always sees a live world
        // matrix for a hand whose real prim did not author a sync behavior.
        //
        // Single-authority: when the loaded stage already carries an authored
        // `*_controller_sync` behavior on the real `/World/Player/Controllers/*`
        // prim (a packaged player), that behavior is the sole pose driver — skip
        // the vestigial proxy for that hand so the two drivers never diverge.
        let left_controller = if controller_behavior_owned[0] {
            None
        } else {
            spawn_controller_entity(world, "__xr__/LeftController")
        };
        let right_controller = if controller_behavior_owned[1] {
            None
        } else {
            spawn_controller_entity(world, "__xr__/RightController")
        };

        let mut platform = XrPlatform::new();
        // Drive the lifecycle to Focused so poses are live. In the mock /
        // desktop path these are pure state transitions; xrPollEvent-driven
        // progression is not wired.
        platform.begin_session()?;
        platform.synchronize()?;
        platform.show()?;
        platform.focus()?;

        Ok(Self {
            platform,
            left_controller,
            right_controller,
            has_left_pose: false,
            has_right_pose: false,
            #[cfg(idtx_xr)]
            xr_session: None,
            #[cfg(feature = "gesture-recognition")]
            gesture: None,
        })
    }

    /// Attach the real graphics-bound `OpenXR` session created by the shell
    /// (which owns the persistent instance and the wgpu D3D12 device). Only
    /// meaningful on the `pcvr` Windows path.
    #[cfg(idtx_xr)]
    pub fn set_graphics_session(&mut self, session: idtx_openxr_sys::XrGraphicsSession) {
        self.xr_session = Some(session);
    }

    /// Mutable access to the XR platform (e.g. to inject a mock pose in
    /// tests, or register a pose-changed callback).
    pub fn platform_mut(&mut self) -> &mut XrPlatform {
        &mut self.platform
    }

    /// Shared access to the XR platform (e.g. to read hand poses).
    pub fn platform(&self) -> &XrPlatform {
        &self.platform
    }

    /// Spawn the background hand-gesture recogniser for this preview.
    ///
    /// Idempotent: a second call replaces the running worker. Once enabled,
    /// each [`Self::tick`] forwards the located hand poses to the worker and
    /// routes recognised gestures into the behavior [`InputRouter`] as
    /// `Gesture/<kind>/<hand>` sources (bind them in `input_map.toml`).
    #[cfg(feature = "gesture-recognition")]
    pub fn enable_gesture_recognition(&mut self, cfg: GestureConfig) {
        self.gesture = Some(GestureRuntime {
            link: idtx_platform_xr::GestureWorker::spawn(cfg),
            prev_sources: std::collections::HashSet::new(),
            frame: 0,
            last_events: Vec::new(),
        });
    }

    /// The gesture events recognised on the most recent [`Self::pump_gestures`].
    ///
    /// Empty when gesture recognition is disabled or nothing fired this frame.
    /// These are in addition to the input-router routing; a shell can consume
    /// them directly (e.g. to drive camera or selection commands).
    #[cfg(feature = "gesture-recognition")]
    #[must_use]
    pub fn last_gesture_events(&self) -> &[GestureEvent] {
        self.gesture
            .as_ref()
            .map_or(&[], |g| g.last_events.as_slice())
    }

    /// Forward this frame's hand poses to the gesture worker, drain recognised
    /// events, and press the active gesture sources onto `frame` as
    /// `Gesture/<kind>/<hand>` sources — exactly as [`apply_xr_controllers`]
    /// feeds `Controller/*` sources. The caller then hands `frame` to
    /// `BehaviorHost::update_input` (one input feed per frame), so bound actions
    /// resolve a pinch/etc. alongside keyboard / controller input. No-op when
    /// recognition is disabled.
    ///
    /// The active set evolves from the previous frame (Started/Held insert,
    /// Ended removes), so a frame that happens to carry no event keeps a held
    /// gesture down rather than flickering.
    ///
    /// [`apply_xr_controllers`]: idtx_behavior::apply_xr_controllers
    #[cfg(feature = "gesture-recognition")]
    pub fn pump_gestures(&mut self, frame: &mut idtx_behavior::FrameInput) {
        // Disjoint field borrows: `&mut self.gesture` + `&self.platform`.
        let Some(g) = self.gesture.as_mut() else {
            return;
        };

        // Forward the latest located poses (set by render_xr / the mock path).
        let (left, right) = self.platform.hand_poses();
        let snap = HandSnapshot {
            left: left.cloned(),
            right: right.cloned(),
            frame_index: g.frame,
        };
        g.frame = g.frame.wrapping_add(1);
        let _ = g.link.send(snap);

        // Evolve the active gesture-source set from last frame's.
        let events = g.link.drain();
        let mut active = g.prev_sources.clone();
        for ev in &events {
            let source = ev.source_string();
            match ev.phase {
                GesturePhase::Ended => {
                    active.remove(&source);
                }
                GesturePhase::Started | GesturePhase::Held => {
                    active.insert(source);
                }
            }
        }

        // Press the active sources; release the ones that ended this frame.
        for source in &active {
            frame.press(source.as_str());
        }
        for source in g.prev_sources.difference(&active) {
            frame.release(source);
        }

        g.prev_sources = active;
        g.last_events = events;
    }

    /// The latest located HMD pose, or `None`.
    #[must_use]
    pub fn current_pose(&self) -> Option<HmdPose> {
        self.platform.current_pose()
    }

    /// Poll the HMD + controllers and feed the located pose snapshot to `host`,
    /// **without** running the host. The single `run_frame` per frame is owned
    /// by [`StudioSession::tick_behaviors`](crate::StudioSession::tick_behaviors)
    /// Returns `true` when a tracked HMD pose was available this frame.
    ///
    /// On the real D3D12 path the stereo frame loop (begin/render/submit) is
    /// driven by `StudioSession::render_xr`, not here.
    pub fn feed_snapshot(&mut self, host: &mut BehaviorHost, recenter: glam::Vec3) -> bool {
        self.platform.tracker_mut().poll();
        feed_xr_snapshot(host, &self.platform, recenter)
    }

    /// Sweep the fallback controller proxy transforms into world matrices.
    /// Call after the host's `run_frame`. The camera is driven by its authored
    /// behavior through the session layer, not swept here.
    pub fn sweep(&mut self, world: &mut World) {
        self.sweep_or_seed_controller(world, true);
        self.sweep_or_seed_controller(world, false);
    }

    /// Begin a stereo XR frame on the real graphics session (Stage C).
    ///
    /// Pumps runtime events first: if the runtime asks the app to exit (headset
    /// removed and timed out, exited via the system menu, or session lost) this
    /// returns [`XrBeginOutcome::Exit`] so the caller can stop the preview.
    #[cfg(idtx_xr)]
    pub fn begin_xr_frame(&mut self) -> XrBeginOutcome {
        let Some(xr) = self.xr_session.as_mut() else {
            return XrBeginOutcome::Exit(None);
        };
        match xr.poll_events() {
            Ok(true) => {}
            Ok(false) => return XrBeginOutcome::Exit(None),
            Err(e) => return XrBeginOutcome::Exit(Some(format!("{e}"))),
        }
        match xr.begin_frame() {
            Ok(Some(frame)) => XrBeginOutcome::Frame(frame),
            Ok(None) => XrBeginOutcome::Skip,
            Err(e) => XrBeginOutcome::Exit(Some(format!("{e}"))),
        }
    }

    /// Submit the stereo XR frame after both eyes are rendered (Stage C).
    ///
    /// `depth_range` is `Some((near, far))` to chain a depth composition layer
    /// (when the runtime supports it and the eyes rendered depth), else `None`
    /// for color-only submission.
    ///
    /// # Errors
    ///
    /// Returns the runtime error message if `xrEndFrame` fails.
    #[cfg(idtx_xr)]
    pub fn submit_xr_frame(
        &mut self,
        rendered: bool,
        depth_range: Option<(f32, f32)>,
    ) -> Result<(), String> {
        if let Some(xr) = self.xr_session.as_mut() {
            xr.submit_frame(rendered, depth_range)
                .map_err(|e| format!("{e}"))?;
        }
        Ok(())
    }

    /// Borrow the real graphics session, if attached (to build GPU resources).
    #[cfg(idtx_xr)]
    #[must_use]
    pub fn graphics_session(&self) -> Option<&idtx_openxr_sys::XrGraphicsSession> {
        self.xr_session.as_ref()
    }

    /// Sweep the controller entity's transform into `GlobalTransform`, with two paths:
    ///
    /// - **WASM path**: if the controller sync behavior ran this frame (`TransformDirty`
    ///   is set), propagate `Transform.matrix → GlobalTransform` and clear the flag.
    /// - **Direct path**: if no behavior ran (`.wasm` not yet committed — deviation 17),
    ///   seed `GlobalTransform` directly from the platform pose so the render
    ///   injection always sees a live world matrix instead of `IDENTITY`.
    ///
    /// In both paths the `has_left_pose` / `has_right_pose` flag is set so that
    /// [`Self::controller_world_matrix`] returns `None` until a real pose has landed,
    /// preventing phantom capsules at the world origin when no controller is tracked.
    fn sweep_or_seed_controller(&mut self, world: &mut World, left: bool) {
        // Extract the entity ID into a plain Copy value so we are not holding a
        // borrow on `self.left_controller` / `self.right_controller` when we later
        // need to mutate other fields of `self`.
        let entity = {
            let ctrl = if left {
                self.left_controller.as_ref()
            } else {
                self.right_controller.as_ref()
            };
            let Some(ctrl) = ctrl else { return };
            ctrl.entity
        };

        // Path 1: WASM behavior ran and marked the entity dirty — propagate it.
        if world.get::<bec::TransformDirty>(entity).is_some() {
            if let Some(local) = world.get::<bec::Transform>(entity).map(|t| t.matrix) {
                if let Some(mut global) = world.get_mut::<bec::GlobalTransform>(entity) {
                    global.0 = local;
                }
            }
            world.entity_mut(entity).remove::<bec::TransformDirty>();
            if left {
                self.has_left_pose = true;
            } else {
                self.has_right_pose = true;
            }
            return;
        }

        // Path 2: No behavior ran — seed GlobalTransform directly from the platform.
        // Extract position/orientation as Copy primitives so the `controller_poses()`
        // borrow on `self.platform` is released before we mutate has_*_pose.
        let (lp, rp) = self.platform.controller_poses();
        let pose_trs = if left {
            lp.map(|p| (p.position, p.orientation))
        } else {
            rp.map(|p| (p.position, p.orientation))
        };
        // `lp` / `rp` borrows end here; only `pose_trs: Option<(Vec3, Quat)>` remains.
        if let Some((pos, rot)) = pose_trs {
            let mat = Mat4::from_rotation_translation(rot, pos);
            if let Some(mut global) = world.get_mut::<bec::GlobalTransform>(entity) {
                global.0 = mat;
            }
            if left {
                self.has_left_pose = true;
            } else {
                self.has_right_pose = true;
            }
        }
    }

    /// The left-controller ECS entity, if present.
    #[must_use]
    pub fn left_controller(&self) -> Option<&ControllerEntity> {
        self.left_controller.as_ref()
    }

    /// The right-controller ECS entity, if present.
    #[must_use]
    pub fn right_controller(&self) -> Option<&ControllerEntity> {
        self.right_controller.as_ref()
    }

    /// The world matrix of a controller entity, or `None` when not spawned or
    /// not yet driven by a pose.
    ///
    /// Returns `None` until the controller has received at least one tracked
    /// pose (`has_left_pose` / `has_right_pose`). This prevents phantom capsule
    /// meshes from rendering at the world origin (i.e., `IDENTITY`) when the
    /// controller is untracked.
    #[must_use]
    pub fn controller_world_matrix(&self, world: &World, left: bool) -> Option<Mat4> {
        if left && !self.has_left_pose {
            return None;
        }
        if !left && !self.has_right_pose {
            return None;
        }
        let ctrl = if left {
            self.left_controller.as_ref()
        } else {
            self.right_controller.as_ref()
        };
        let ctrl = ctrl?;
        world.get::<bec::GlobalTransform>(ctrl.entity).map(|g| g.0)
    }

    /// The world matrix of a controller **aim** pose (pointer-ray source), or
    /// `None` when that hand's aim pose is untracked this frame.
    ///
    /// Unlike [`Self::controller_world_matrix`], the aim pose is not backed by an
    /// ECS entity — it is a transient per-frame pose read straight from the
    /// platform (STAGE / render space, same frame as the grip pose), used only to
    /// author the pointer prim's pose Xform. Returns `None` when untracked so the
    /// pointer keeps its last authored pose rather than snapping to the origin.
    #[must_use]
    pub fn aim_world_matrix(&self, left: bool) -> Option<Mat4> {
        let (lp, rp) = self.platform.aim_poses();
        let pose = if left { lp } else { rp }?;
        Some(Mat4::from_rotation_translation(
            pose.orientation,
            pose.position,
        ))
    }

    /// Re-seed the fallback controller proxy ECS entities into a *different*
    /// world after the owning [`StudioSession`] was swapped (a level
    /// transition).
    ///
    /// The bridge-only proxy entities spawned by [`Self::start`] live in one
    /// session's ECS world; when the runtime carries this preview onto a
    /// freshly-loaded level (a new `StudioSession`, a new world), their old
    /// `Entity` handles are stale and must not be read against the new world.
    /// Respawn them in `world` and clear the pose latches so the next located
    /// pose re-seeds them. `controller_behavior_owned` mirrors [`Self::start`]:
    /// a hand whose real prim authored a `*_controller_sync` behavior owns its
    /// grip pose, so no vestigial proxy is spawned for it. The real `OpenXR`
    /// session (`xr_session`) and the platform tracker state are untouched —
    /// only the per-world proxies move.
    pub fn reseed_controllers(&mut self, world: &mut World, controller_behavior_owned: [bool; 2]) {
        self.left_controller = if controller_behavior_owned[0] {
            remove_controller_proxy(world, "__xr__/LeftController");
            None
        } else {
            spawn_controller_entity(world, "__xr__/LeftController")
        };
        self.right_controller = if controller_behavior_owned[1] {
            remove_controller_proxy(world, "__xr__/RightController");
            None
        } else {
            spawn_controller_entity(world, "__xr__/RightController")
        };
        // The new proxies have no pose yet; guard `controller_world_matrix`
        // until the next located pose arrives (same as a fresh `start`).
        self.has_left_pose = false;
        self.has_right_pose = false;
    }

    /// Tear the session down, driving the lifecycle back to `Idle`. Best
    /// effort — a transition error is swallowed so shutdown never fails.
    pub fn end(&mut self) {
        #[cfg(idtx_xr)]
        if let Some(xr) = self.xr_session.as_mut() {
            xr.request_exit();
            // Drain the STOPPING/EXITING events so the session ends cleanly.
            let _ = xr.poll_events();
        }
        let _ = self.platform.request_exit();
        let _ = self.platform.end_session();
    }
}

impl Drop for XrPreviewSession {
    fn drop(&mut self) {
        self.end();
    }
}

// ── Bridge-only fallback controller helpers ─────────────────────────────────────

/// Spawn a bridge-only fallback ECS entity for a controller. The entity
/// carries `Transform`, `GlobalTransform`, and `UsdPath` so its pose can be
/// seeded from the platform each frame; no behavior is attached and no USD
/// prim is authored. Always returns `Some(ControllerEntity)`.
#[derive(Component)]
struct ControllerProxy;

fn remove_controller_proxy(world: &mut World, path: &str) {
    let entities: Vec<_> = world.query::<(Entity, &bec::UsdPath, &ControllerProxy)>().iter(world)
        .filter(|(_, candidate, _)| candidate.0 == path).map(|(entity, ..)| entity).collect();
    for entity in entities { world.despawn(entity); }
}

fn spawn_controller_entity(world: &mut World, path: &str) -> Option<ControllerEntity> {
    let existing: Vec<_> = world.query::<(Entity, &bec::UsdPath, &ControllerProxy)>().iter(world)
        .filter(|(_, candidate, _)| candidate.0 == path).map(|(entity, ..)| entity).collect();
    if let Some(&entity) = existing.first() {
        for duplicate in &existing[1..] { world.despawn(*duplicate); }
        return Some(ControllerEntity { entity, path: path.into() });
    }
    let entity = world
        .spawn((
            ControllerProxy,
            bec::Transform::default(),
            bec::GlobalTransform::default(),
            bec::UsdPath(path.to_owned()),
        ))
        .id();

    Some(ControllerEntity {
        entity,
        path: path.to_owned(),
    })
}

// ── HMD / controller pose feed ──────────────────────────────

/// Feed the XR platform's current HMD pose — and any tracked controller
/// poses — into the behavior host's snapshot. Returns `true` when a tracked
/// HMD pose was available (and the snapshot now carries it), `false` when no
/// HMD pose is tracked (the snapshot is reset to untracked so stale poses
/// never linger).
///
/// Controller poses (left / right) are populated independently; a missing
/// controller does not affect the HMD return value.
///
/// `recenter` is the play-space recenter offset (stage-space ground vector): it
/// is pushed to the host (so a pawn-root behavior re-bases by it) and subtracted
/// from the position of every fed pose (HMD, grips, aims, hand joints). Because
/// the pawn root carries `+recenter` and these child poses carry `−recenter`, the
/// hydrated camera / controllers / hands stay world-fixed while the body
/// re-bases under the headset (see the session's `recenter_step`). Pass
/// `Vec3::ZERO` to disable.
pub fn feed_xr_snapshot(host: &mut BehaviorHost, xr: &XrPlatform, recenter: glam::Vec3) -> bool {
    host.set_xr_recenter(recenter);
    let hmd_tracked = xr.current_pose().is_some();
    let mut snap = match xr.current_pose() {
        Some(mut pose) => {
            pose.position -= recenter;
            XrSnapshot::from(&pose)
        }
        None => XrSnapshot::untracked(),
    };

    // Populate controller poses. Both are in STAGE space (same frame as HMD).
    let (left, right) = xr.controller_poses();
    for (space, pose) in [(XrSpace::LeftHand, left), (XrSpace::RightHand, right)] {
        if let Some(p) = pose {
            let xform = recenter_compensate(
                Xform::from_trs(p.position, p.orientation, glam::Vec3::ONE),
                recenter,
            );
            snap.set(space, XrPoseResult::tracked(xform));
        }
    }

    // Populate aim poses (pointer ray origin+orientation) into the dedicated
    // LeftAim/RightAim channels the `*_aim_sync` behaviors read. Same STAGE
    // frame as the grip poses; independent of them.
    let (left_aim, right_aim) = xr.aim_poses();
    for (space, pose) in [(XrSpace::LeftAim, left_aim), (XrSpace::RightAim, right_aim)] {
        if let Some(p) = pose {
            let xform = recenter_compensate(
                Xform::from_trs(p.position, p.orientation, glam::Vec3::ONE),
                recenter,
            );
            snap.set(space, XrPoseResult::tracked(xform));
        }
    }

    // Populate hand-joint arrays when available, and — in hands-free
    // mode (no controller tracked for that side) — synthesise the grip/aim pose
    // spaces from the tracked hand so controller-oriented behaviors (`grab_prim`,
    // the `*_aim_sync` behaviors) work off hand tracking too. WRIST drives the
    // grip space; PALM drives the aim space. A present controller pose always
    // wins (we only fill a space left invalid above).
    let (left_pose, right_pose) = xr.hand_poses();
    for (side, hand_pose, grip_space, aim_space) in [
        (XrSide::Left, left_pose, XrSpace::LeftHand, XrSpace::LeftAim),
        (XrSide::Right, right_pose, XrSpace::RightHand, XrSpace::RightAim),
    ] {
        let Some(p) = hand_pose else {
            continue;
        };
        snap.set_hand_joints(side, Some(hand_pose_to_wire(p, recenter)));

        if !snap.get(grip_space).is_valid() {
            let wrist = p.joints[1]; // WRIST
            let xform = recenter_compensate(
                Xform::from_trs(wrist.position, wrist.orientation, glam::Vec3::ONE),
                recenter,
            );
            snap.set(grip_space, XrPoseResult::tracked(xform));
        }
        if !snap.get(aim_space).is_valid() {
            let palm = p.joints[0]; // PALM
            let xform = recenter_compensate(
                Xform::from_trs(palm.position, palm.orientation, glam::Vec3::ONE),
                recenter,
            );
            snap.set(aim_space, XrPoseResult::tracked(xform));
        }
    }

    // Record which source(s) drive each side so a behavior can distinguish a
    // physical controller from hand tracking — the grip/aim pose spaces above
    // may be synthesised from the hand (hands-free mode), which hides the source
    // from a behavior that only probes `get_pose`. Controller presence is the
    // raw controller grip OR aim feed; hand presence is the hand-skeleton feed.
    for (side, controller, aim, hand) in [
        (XrSide::Left, left, left_aim, left_pose),
        (XrSide::Right, right, right_aim, right_pose),
    ] {
        let source =
            classify_input_source(controller.is_some() || aim.is_some(), hand.is_some());
        snap.set_input_source(side, source);
    }

    host.set_xr_snapshot(snap);
    hmd_tracked
}

/// Classify a side's XR input source from the raw platform feeds: whether any
/// controller pose (grip or aim) is present, and whether a hand skeleton is
/// present. Both bits can be set at once when a runtime reports both; neither
/// yields [`XrInputSource::NONE`]. Pure, so it is unit-tested directly without a
/// live XR platform or behavior host.
fn classify_input_source(controller_present: bool, hand_present: bool) -> XrInputSource {
    XrInputSource::NONE
        .with_controller(controller_present)
        .with_hand(hand_present)
}

/// Subtract the play-space `recenter` ground offset from an [`Xform`]'s
/// translation (rotation + scale untouched). The per-pose half of the recenter:
/// the pawn root carries `+recenter`, so a child pose carrying `−recenter` lands
/// at its pre-recenter world position. `recenter == ZERO` is the identity.
fn recenter_compensate(xform: Xform, recenter: glam::Vec3) -> Xform {
    Xform {
        translation: (glam::Vec3::from(xform.translation) - recenter).into(),
        ..xform
    }
}

/// Convert a [`idtx_platform_xr::HandPose`] to the 26-element
/// [`XrHandJointResult`] array the behavior ABI expects, subtracting the
/// play-space `recenter` ground offset from every joint position (the hand
/// skeleton follows the body re-base, like the grip/aim poses).
fn hand_pose_to_wire(
    pose: &idtx_platform_xr::HandPose,
    recenter: glam::Vec3,
) -> Box<[XrHandJointResult; XR_HAND_JOINT_COUNT]> {
    const ZERO: XrHandJointResult = XrHandJointResult {
        position: [0.0; 3],
        radius: 0.0,
        orientation: [0.0, 0.0, 0.0, 1.0],
        valid: 0,
        _pad: 0,
    };
    let mut out = Box::new([ZERO; XR_HAND_JOINT_COUNT]);
    for (i, j) in pose.joints.iter().enumerate() {
        out[i] = XrHandJointResult {
            position: (j.position - recenter).to_array(),
            radius: j.radius,
            orientation: [
                j.orientation.x,
                j.orientation.y,
                j.orientation.z,
                j.orientation.w,
            ],
            valid: 1,
            _pad: 0,
        };
    }
    out
}

/// Probe for an available `OpenXR` runtime.
///
/// Returns the loaded [`XrLoader`] when a runtime is present, or `None`
/// otherwise. Call once at startup and cache the result; the UI gates the
/// "Play in `OpenXR`" button on `Some`.
#[must_use]
pub fn probe_xr_runtime() -> Option<XrLoader> {
    let loader = XrLoader::try_load().ok()?;
    match loader.available_runtimes() {
        Ok(runtimes) if !runtimes.is_empty() => Some(loader),
        _ => None,
    }
}

/// Like [`probe_xr_runtime`] but returns a human-readable diagnostic string
/// alongside the result — useful for surfacing errors in the status bar when
/// the user clicks the manual Refresh button.
#[must_use]
pub fn probe_xr_runtime_diag() -> (Option<XrLoader>, String) {
    let loader = match XrLoader::try_load() {
        Ok(l) => l,
        Err(e) => return (None, format!("OpenXR loader failed: {e}")),
    };
    match loader.available_runtimes() {
        Err(e) => (None, format!("OpenXR instance probe failed: {e}")),
        Ok(runtimes) if runtimes.is_empty() => (
            None,
            "OpenXR runtime registered but reported no runtimes.".to_owned(),
        ),
        Ok(runtimes) => {
            let msg = format!(
                "OpenXR runtime: {} {}",
                runtimes[0].name, runtimes[0].version
            );
            (Some(loader), msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lift's correctness-critical primitive: a level swap carries the
    /// runtime-owned `XrPreviewSession` onto a freshly-loaded level (a new ECS
    /// world), so its controller proxy entities must be re-seeded there — the
    /// old world's `Entity` handles are stale. Exercised host-side via `mock-xr`
    /// (no headset); the swapchain GPU carry is `idtx_xr`-only and is covered by
    /// the Quest/visionOS cross-builds.
    #[cfg(feature = "mock-xr")]
    #[test]
    fn reseed_controllers_moves_proxies_to_a_new_world() {
        let loader = XrLoader::try_load().expect("mock loader loads");
        let mut world_a = World::new();
        let mut preview = XrPreviewSession::start(&loader, &mut world_a, [false, false])
            .expect("preview starts on the mock runtime");

        let left_a = preview.left_controller().expect("left proxy spawned").entity;
        let right_a = preview
            .right_controller()
            .expect("right proxy spawned")
            .entity;
        assert!(world_a.entities().contains(left_a));
        assert!(world_a.entities().contains(right_a));

        // Simulate a level swap: re-seed into a brand-new world (the freshly
        // loaded level's ECS world). The old handles must not be reused there.
        let mut world_b = World::new();
        preview.reseed_controllers(&mut world_b, [false, false]);

        let left_b = preview.left_controller().expect("left re-seeded").entity;
        let right_b = preview.right_controller().expect("right re-seeded").entity;
        assert!(
            world_b.entities().contains(left_b),
            "re-seeded left proxy must live in the new world"
        );
        assert!(
            world_b.entities().contains(right_b),
            "re-seeded right proxy must live in the new world"
        );
        // Pose latches reset so the capsule is suppressed until a real pose lands.
        assert!(!preview.has_left_pose);
        assert!(!preview.has_right_pose);

        // A hand whose real prim owns a `*_controller_sync` behavior spawns no
        // vestigial proxy on re-seed (single-authority, mirrors `start`).
        let mut world_c = World::new();
        preview.reseed_controllers(&mut world_c, [true, true]);
        assert!(preview.left_controller().is_none());
        assert!(preview.right_controller().is_none());
    }

    /// `recenter_compensate` subtracts the ground offset from the translation
    /// and leaves rotation + scale untouched; `ZERO` is the identity.
    #[test]
    fn recenter_compensate_subtracts_translation_only() {
        let rot = glam::Quat::from_rotation_y(0.7);
        let xform = Xform::from_trs(glam::Vec3::new(1.0, 2.0, 3.0), rot, glam::Vec3::splat(2.0));

        // ZERO offset is a no-op.
        let same = recenter_compensate(xform, glam::Vec3::ZERO);
        assert_eq!(same, xform);

        // A ground offset shifts translation by -offset; rotation/scale survive.
        let s = glam::Vec3::new(0.5, 0.0, -0.25);
        let out = recenter_compensate(xform, s);
        let t: glam::Vec3 = out.translation.into();
        assert!((t - glam::Vec3::new(0.5, 2.0, 3.25)).length() < 1e-6, "t = {t:?}");
        assert_eq!(out.rotation, xform.rotation);
        assert_eq!(out.scale, xform.scale);
    }

    // mock-xr tests: inject controller poses and verify the
    // platform and snapshot surface them correctly.

    #[cfg(feature = "mock-xr")]
    #[test]
    fn controller_poses_round_trip_platform() {
        use glam::{Quat, Vec3};
        use idtx_platform_xr::{ControllerPose, XrPlatform};

        let mut xr = XrPlatform::new();
        let left_pos = Vec3::new(0.3, 1.2, -0.5);
        let right_pos = Vec3::new(-0.3, 1.1, -0.4);
        xr.inject_controller_poses(
            Some(ControllerPose {
                position: left_pos,
                orientation: Quat::IDENTITY,
                timestamp: 0.0,
            }),
            Some(ControllerPose {
                position: right_pos,
                orientation: Quat::IDENTITY,
                timestamp: 0.0,
            }),
        );
        let (left, right) = xr.controller_poses();
        let l = left.expect("left controller tracked after injection");
        let r = right.expect("right controller tracked after injection");
        assert!(
            (l.position - left_pos).length() < 1e-6,
            "left position round-trips"
        );
        assert!(
            (r.position - right_pos).length() < 1e-6,
            "right position round-trips"
        );
    }

    #[cfg(feature = "mock-xr")]
    #[test]
    fn feed_snapshot_populates_controller_poses() {
        use glam::{Quat, Vec3};
        use idtx_behavior::XrSnapshot;
        use idtx_behavior::abi::{Xform, XrPoseResult, XrSpace};
        use idtx_platform_xr::{ControllerPose, XrPlatform};

        let mut xr = XrPlatform::new();
        let left_pos = Vec3::new(0.5, 1.5, -0.5);
        xr.inject_controller_poses(
            Some(ControllerPose {
                position: left_pos,
                orientation: Quat::IDENTITY,
                timestamp: 0.0,
            }),
            None,
        );

        // Replicate feed_xr_snapshot's snapshot-building logic to verify
        // that controller poses are correctly encoded into XrPoseResult.
        let mut snap = XrSnapshot::untracked();
        let (left, right) = xr.controller_poses();
        for (space, pose) in [(XrSpace::LeftHand, left), (XrSpace::RightHand, right)] {
            if let Some(p) = pose {
                let xform = Xform::from_trs(p.position, p.orientation, glam::Vec3::ONE);
                snap.set(space, XrPoseResult::tracked(xform));
            }
        }

        let l = snap.get(XrSpace::LeftHand);
        assert_eq!(l.valid, 1, "injected left hand must be tracked in snapshot");
        assert_eq!(
            snap.get(XrSpace::RightHand).valid,
            0,
            "uninjected right hand must be untracked"
        );
        let t: Vec3 = l.xform.translation.into();
        assert!(
            (t - left_pos).length() < 1e-5,
            "translation round-trips through Xform"
        );
    }

    #[cfg(feature = "mock-xr")]
    #[test]
    fn feed_snapshot_populates_aim_poses_independent_of_grip() {
        use glam::{Quat, Vec3};
        use idtx_behavior::XrSnapshot;
        use idtx_behavior::abi::{Xform, XrPoseResult, XrSpace};
        use idtx_platform_xr::{ControllerPose, XrPlatform};

        let mut xr = XrPlatform::new();
        // Grip and aim differ so we can prove they land in distinct channels.
        let grip_pos = Vec3::new(0.5, 1.5, -0.5);
        let aim_pos = Vec3::new(0.5, 1.6, -1.5);
        xr.inject_controller_poses(
            Some(ControllerPose {
                position: grip_pos,
                orientation: Quat::IDENTITY,
                timestamp: 0.0,
            }),
            None,
        );
        xr.inject_aim_poses(
            Some(ControllerPose {
                position: aim_pos,
                orientation: Quat::IDENTITY,
                timestamp: 0.0,
            }),
            None,
        );

        // Replicate feed_xr_snapshot's grip+aim snapshot-building logic.
        let mut snap = XrSnapshot::untracked();
        let (left, right) = xr.controller_poses();
        for (space, pose) in [(XrSpace::LeftHand, left), (XrSpace::RightHand, right)] {
            if let Some(p) = pose {
                let xform = Xform::from_trs(p.position, p.orientation, glam::Vec3::ONE);
                snap.set(space, XrPoseResult::tracked(xform));
            }
        }
        let (left_aim, right_aim) = xr.aim_poses();
        for (space, pose) in [(XrSpace::LeftAim, left_aim), (XrSpace::RightAim, right_aim)] {
            if let Some(p) = pose {
                let xform = Xform::from_trs(p.position, p.orientation, glam::Vec3::ONE);
                snap.set(space, XrPoseResult::tracked(xform));
            }
        }

        let aim = snap.get(XrSpace::LeftAim);
        assert_eq!(aim.valid, 1, "injected left aim must be tracked");
        let at: Vec3 = aim.xform.translation.into();
        assert!(
            (at - aim_pos).length() < 1e-5,
            "aim translation round-trips"
        );
        // Grip channel on the same hand carries the *grip* pose, not the aim.
        let grip: Vec3 = snap.get(XrSpace::LeftHand).xform.translation.into();
        assert!(
            (grip - grip_pos).length() < 1e-5,
            "grip channel is independent of aim"
        );
        assert_eq!(
            snap.get(XrSpace::RightAim).valid,
            0,
            "uninjected right aim stays untracked"
        );
    }

    #[cfg(feature = "mock-xr")]
    #[test]
    fn feed_snapshot_falls_back_to_hand_grip_aim_when_no_controller() {
        use glam::Vec3;
        use idtx_behavior::XrSnapshot;
        use idtx_behavior::abi::{Xform, XrPoseResult, XrSpace};
        use idtx_platform_xr::{HandPose, XrPlatform};

        // Right hand tracked, no controllers — the hands-free case.
        let mut xr = XrPlatform::new();
        let mut pose = HandPose::zeroed();
        let palm = Vec3::new(0.1, 1.2, -0.3); // joint 0 → aim space
        let wrist = Vec3::new(0.1, 1.1, -0.2); // joint 1 → grip space
        pose.joints[0].position = palm;
        pose.joints[1].position = wrist;
        xr.inject_hand_poses(None, Some(pose));

        // Replicate feed_xr_snapshot's controller+hand branch: controllers are
        // absent, so the grip/aim spaces fall back to the tracked hand (WRIST →
        // grip, PALM → aim). Mirrors the neighbouring replicate-style tests.
        let mut snap = XrSnapshot::untracked();
        let (_left, right) = xr.hand_poses();
        if let Some(p) = right {
            if !snap.get(XrSpace::RightHand).is_valid() {
                let w = p.joints[1];
                snap.set(
                    XrSpace::RightHand,
                    XrPoseResult::tracked(Xform::from_trs(w.position, w.orientation, Vec3::ONE)),
                );
            }
            if !snap.get(XrSpace::RightAim).is_valid() {
                let pl = p.joints[0];
                snap.set(
                    XrSpace::RightAim,
                    XrPoseResult::tracked(Xform::from_trs(pl.position, pl.orientation, Vec3::ONE)),
                );
            }
        }

        let grip = snap.get(XrSpace::RightHand);
        assert_eq!(grip.valid, 1, "hand-tracked right grip must be populated");
        let gt: Vec3 = grip.xform.translation.into();
        assert!((gt - wrist).length() < 1e-5, "grip space comes from the WRIST");
        let at: Vec3 = snap.get(XrSpace::RightAim).xform.translation.into();
        assert!((at - palm).length() < 1e-5, "aim space comes from the PALM");
        // The untracked left hand leaves its spaces alone.
        assert_eq!(snap.get(XrSpace::LeftHand).valid, 0, "no left hand → no grip");
    }

    #[test]
    fn classify_input_source_covers_all_combinations() {
        // No feed → NONE / not tracked.
        let none = classify_input_source(false, false);
        assert_eq!(none, XrInputSource::NONE);
        assert!(!none.is_tracked());

        // Controller only.
        let ctrl = classify_input_source(true, false);
        assert!(ctrl.has_controller() && !ctrl.has_hand());

        // Hand only — the hands-free case that `get_pose` alone can't reveal.
        let hand = classify_input_source(false, true);
        assert!(hand.has_hand() && !hand.has_controller());

        // Both at once (a runtime reporting controller + hand tracking).
        let both = classify_input_source(true, true);
        assert!(both.has_controller() && both.has_hand() && both.is_tracked());
    }

    /// Verifies the invariant exercised by `sweep_controller_transform`:
    /// a `Transform` marked dirty must propagate to `GlobalTransform` and
    /// clear the dirty marker. Private method; logic inlined for isolation.
    #[test]
    fn sweep_transform_dirty_propagates_to_global() {
        use glam::{Mat4, Vec3};
        use idtx_behavior::ecs as bec;
        use idtx_scene::bevy_ecs::world::World;

        let mut world = World::new();
        let entity = world
            .spawn((
                bec::Transform::default(),
                bec::GlobalTransform::default(),
                bec::UsdPath("/__xr_ctrl_sweep_test__".to_owned()),
            ))
            .id();

        let expected = Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0));
        world.get_mut::<bec::Transform>(entity).unwrap().matrix = expected;
        world.entity_mut(entity).insert(bec::TransformDirty);
        assert!(
            world.get::<bec::TransformDirty>(entity).is_some(),
            "dirty must be set"
        );

        // Inlined from sweep_or_seed_controller (private method) — WASM path only.
        if let Some(local) = world.get::<bec::Transform>(entity).map(|t| t.matrix) {
            if let Some(mut global) = world.get_mut::<bec::GlobalTransform>(entity) {
                global.0 = local;
            }
        }
        world.entity_mut(entity).remove::<bec::TransformDirty>();

        assert_eq!(
            world.get::<bec::GlobalTransform>(entity).unwrap().0,
            expected
        );
        assert!(
            world.get::<bec::TransformDirty>(entity).is_none(),
            "dirty must be cleared"
        );
    }
}
