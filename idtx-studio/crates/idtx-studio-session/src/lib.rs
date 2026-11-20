//! One-tab `StageService`, scene, and renderer integration controller.

/// Session-owned behavior host attach logic. The WASM behavior
/// host is always compiled in — behaviors drive the plain runtime player, not
/// just the XR preview.
mod behavior;
mod runtime_prototypes;
mod presentation;
mod invalidation;
#[cfg(all(feature = "remote", feature = "authoring"))]
mod collab_prepare;
#[cfg(all(feature = "remote", feature = "authoring"))]
mod collab_attributes;
/// Runtime-only rigid-body physics subsystem (`UsdPhysics` → Rapier). Gated by
/// the `physics` feature, which only `idtx-runtime-core` enables — the
/// in-process editor viewport never simulates (plan §2/§3).
#[cfg(feature = "physics")]
mod physics;
#[cfg(feature = "openexec")]
mod exec;
/// The shared network-egress SSRF gate. Lives under `remote` (not `openexec`)
/// because the behavior `net` capability needs it without the compute
/// subsystem; `openexec` enables `remote`, so stage REST datasources still see
/// it. Kept as the public name `ExecNetworkPolicy` this crate has always
/// exported.
#[cfg(feature = "remote")]
pub use idtx_net_guard::NetworkPolicy as ExecNetworkPolicy;
/// Session-side scene-query backend for the behavior host (raycast / arc-cast /
/// prim-type query).
mod scene_backend_impl;
/// Fallback capsule meshes for XR controller visualisation. Legacy
/// mesh-bake path — gated behind `xr-bake` (off by default; the default path
/// hydrates controllers as visible prefabs instead — see `xr_controller_prefab`).
#[cfg(feature = "xr-bake")]
mod xr_controller_mesh;
/// Normal-hydration controller prefab authoring (session-layer). Pure
/// string/coordinate authoring with no XR
/// dependencies, so it is available regardless of `xr-preview` — the shell's
/// `preview_assembly` reuses its pose-block generator for the on-disk sublayer.
pub mod xr_controller_prefab;
/// Bone/joint meshes for XR hand-skeleton visualisation.
#[cfg(feature = "xr-preview")]
mod xr_hand_mesh;
/// Per-XR-frame performance instrumentation (rolling aggregation + JSON dump).
#[cfg(feature = "metrics")]
pub mod xr_metrics;
/// `OpenXR` preview seam — feeds HMD poses into the behavior host.
#[cfg(feature = "xr-preview")]
pub mod xr_preview;
/// Stage C stereo headset rendering (real D3D12/Vulkan graphics path).
#[cfg(idtx_xr)]
#[allow(clippy::doc_markdown)]
mod xr_render;
/// Strategy-B `OpenXR`-dictated Vulkan device bring-up (Quest/Android): the
/// runtime creates the `VkInstance`/`VkDevice`, wrapped into wgpu via wgpu-hal.
#[cfg(all(idtx_xr, target_os = "android"))]
#[allow(clippy::doc_markdown)]
pub mod xr_vulkan;
/// Apple Vision Pro per-eye rendering into `CompositorServices` drawables
/// (the Metal mirror of [`xr_render`]). visionOS shares no code with `OpenXR`.
#[cfg(all(feature = "visionos", target_vendor = "apple"))]
pub mod visionos_render;

// Re-exports so the shell (and other embedders) can drive behaviors / an XR
// preview with only an `idtx-studio-session` dependency.
pub use idtx_behavior::BehaviorManifest;
// The single behavior host + its transport primitives, re-exported so an
// embedder (and the headless preview test) can build/drive the host with only
// an `idtx-studio-session` dependency — no direct `idtx-behavior` dependency.
pub use idtx_behavior::{
    BehaviorHost, FrameInput, InputMap, InputRouter, Limits, MappingContext, MappingContextStack,
    PhysicsControlCommand, PlayState, PlaybackCommand, PlaybackCommandKind, RawInput,
    SessionEditCommand, SessionEditOp, StageEvent,
};
#[cfg(feature = "xr-preview")]
pub use idtx_openxr_sys::XrControllerKind;
#[cfg(feature = "xr-preview")]
pub use idtx_openxr_sys::XrLoader;
// Persistent graphics-bound instance + session (real XR path: D3D12 on Windows
// PCVR, Vulkan on Quest/Android).
#[cfg(idtx_xr)]
pub use idtx_openxr_sys::{XrGraphicsSession, XrInstance};
// Quest: hand the real Android Activity jobject to the OpenXR instance-create
// path (ndk_context only carries the Application; see `set_android_activity`).
#[cfg(all(idtx_xr, target_os = "android"))]
pub use idtx_openxr_sys::set_android_activity;
// Redirect the XR-metrics JSON dump out of the system temp dir (not
// `adb pull`-able on Android) into a retrievable location; see `xr_metrics`.
#[cfg(feature = "metrics")]
pub use xr_metrics::set_metrics_output_dir;
#[cfg(feature = "xr-preview")]
pub use xr_hand_mesh::XrHandVisualization;
#[cfg(feature = "xr-preview")]
pub use xr_preview::{XrPreviewError, probe_xr_runtime, probe_xr_runtime_diag};

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use glam::{Mat4, Vec3};

use idtx_render::{AssetReadFn, FrameTarget, HudElement, RenderCameraDesc, RenderWorldSnapshot, Renderer};

pub use idtx_render::DownscaleAlgo;
pub use idtx_render::TextureDecodeConfig;

impl StudioSession {
    /// Effective texture policy carried into every subsequent level open.
    pub fn texture_config(&self) -> TextureDecodeConfig { self.renderer.texture_config() }
}
pub use idtx_render::TextureDecodeTimings;
/// Re-exported so callers compiled with the `metrics` feature don't need a
/// direct `idtx-render` dependency to destructure the per-frame timing record.
#[cfg(feature = "metrics")]
pub use idtx_render::metrics::RenderFrameTimings;
#[cfg(feature = "editor-overlay")]
use idtx_scene::SkeletonPoseComp;
pub use idtx_scene::{
    AnimClipPlayer, ClipPlayhead, ClipSampleRequest, PlaybackRequest, StageAnimClip, StagePlayback,
};
use idtx_scene::{
    BoundsComp, HudComp, HydrationError, PackedStageMetadata, PopulatorRegistry, PrimNode,
    RenderPurpose, SceneHydrator, SkeletonTopologyComp, VisibilityComp, WorldTransform, Xformable,
    coordinate::{UpAxis, WorldUnitScale, pseudo_root_matrix},
    stable_prim_hash,
};
// Behavior + player-role vocabulary, re-exported so the shell can construct the
// argument types for `set_behavior_kind`/`set_behavior_scope` and read the token
// returned by `resolved_player_role` without taking a direct `idtx-scene`
// dependency (the shell depends on the session, not on scene).
pub use idtx_scene::{BehaviorKind, BehaviorScope, NetRole};
#[cfg(feature = "remote")]
use idtx_usd::StageChangeBatch;
use idtx_usd::{DirtyDomains, StageError, StageMetadata, StageService, TopologyDelta};
use thiserror::Error;

pub use idtx_scene::coordinate::WorldUnitScale as RenderWorldUnitScale;

// ── Session identity ──────────────────────────────────────────────────────────

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Unique identifier for a [`StudioSession`]. Monotonically increasing within
/// the process lifetime; never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(u64);

impl SessionId {
    fn next() -> Self {
        Self(SESSION_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

// ── Stage-level physics authoring ─────────────────────────────────────────────

/// Pseudo-root-relative path of the `PhysicsScene` prim authored by
/// [`StudioSession::activate_physics`] — matches the canonical physics fixtures.
#[cfg(feature = "authoring")]
const PHYSICS_SCENE_DEFAULT_PATH: &str = "/physicsScene";

/// Absolute path of the stage-level behavior prim — a root `def Scope` whose
/// sole purpose is to carry `IdtxBehaviorAPI` + the `idtx:behavior:*` properties
/// for a stage-wide behavior. Named in the root layer's
/// `customLayerData["idtx:stageBehavior"]`.
pub const STAGE_BEHAVIOR_PRIM_PATH: &str = "/StageBehavior";

/// The bare name of the stage-behavior prim, as stored in
/// `customLayerData["idtx:stageBehavior"]` and appended to the pseudo-root to
/// form [`STAGE_BEHAVIOR_PRIM_PATH`].
pub const STAGE_BEHAVIOR_NAME: &str = "StageBehavior";

/// Parse a `vector3f`/`float3`/`double3` `value_repr` of the form `"(x, y, z)"`
/// into `[f32; 3]` (mirrors the runtime physics subsystem's `read_vec3`).
#[cfg(feature = "authoring")]
fn parse_vec3_repr(repr: &str) -> Option<[f32; 3]> {
    let inner = repr.trim().trim_matches(|c| c == '(' || c == ')');
    let mut it = inner.split(',').map(|p| p.trim().parse::<f32>());
    let x = it.next()?.ok()?;
    let y = it.next()?.ok()?;
    let z = it.next()?.ok()?;
    Some([x, y, z])
}

/// UI-neutral snapshot of the stage's `UsdPhysicsScene` state, read once per
/// frame for the Stage inspector panel's Physics section.
///
/// `active` is `true` when a `PhysicsScene`-typed prim is present (the sole gate
/// that enables runtime simulation — see the `physics` subsystem); the gravity
/// fields hold the authored `physics:gravityDirection` / `physics:gravityMagnitude`
/// when active, or the schema defaults `(0, -1, 0)` / `9.81` otherwise.
#[cfg(feature = "authoring")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicsSceneState {
    pub active: bool,
    pub gravity_direction: [f32; 3],
    pub gravity_magnitude: f32,
}

#[cfg(feature = "authoring")]
impl Default for PhysicsSceneState {
    fn default() -> Self {
        Self {
            active: false,
            gravity_direction: [0.0, -1.0, 0.0],
            gravity_magnitude: 9.81,
        }
    }
}


// ── Session source ──────────────────────────────────────────────────────────

/// Where a [`StudioSession`]'s stage was opened from.
///
/// Distinguishes a stage loaded from the local filesystem — which can be saved
/// in place — from one fetched over HTTP(S) through the remote asset resolver,
/// which is read-only from the shell's perspective (Poser must "Save As…" to a
/// local path; see the save-as restriction).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSource {
    /// Opened from a local filesystem path.
    Local(PathBuf),
    /// Opened from a remote server; the stage's root layer identifier is the
    /// original HTTP(S) URL, resolved and cached by `idtx-usd-resolver-http`.
    Remote { url: String },
}

impl SessionSource {
    /// `true` when the stage was opened from a remote server.
    #[must_use]
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }
}

/// A file the collaboration server reloaded, surfaced to the shell so it can
/// re-fetch and reload it. Produced from a
/// [`RemoteEditKind::LayerReloaded`](idtx_collab::client::RemoteEditKind::LayerReloaded)
/// frame.
#[cfg(feature = "remote")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingLayerReload {
    /// Server canonical uploads-relative path of the reloaded file (as on the
    /// `Handshake`): decoded, without `./` or repeated `/`.
    pub usd_path: String,
    /// Server-relative download URI of that file
    /// (`/api/v1/download/<percent-encoded path>`), for matching against the
    /// layer identifiers the stage holds.
    pub usd_uri: String,
}

/// Live status of a remote sync (collaboration) session, for viewport display.
///
/// Only meaningful while a [`CollabClient`](idtx_collab::client::CollabClient)
/// is attached; [`StudioSession::collab_sync_status`] returns `None` otherwise.
#[cfg(feature = "remote")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollabSyncStatus {
    /// Connected, but the server has not yet delivered `SnapshotComplete`, so the
    /// client does not hold the full current state and must not broadcast yet.
    /// Rendered AMBER.
    Syncing,
    /// Joined a session created by another peer ("Connect Remote"), connection
    /// live and snapshot received. Rendered BLUE.
    ConnectedToPeer,
    /// Created and owns a single- or multi-edit session ("Open Remote with
    /// Sync"), connection live and snapshot received. Rendered GREEN.
    Hosting,
    /// A collab client is attached but its connection was lost (server-side
    /// disconnect, idle timeout, or network error). Rendered RED.
    Disconnected,
}

/// Pure classifier mapping the collab connection facts — whether the I/O thread
/// is alive, whether the join snapshot has completed, and whether this client
/// owns the session — onto a [`CollabSyncStatus`]. Factored out for unit testing
/// without a live socket.
#[cfg(feature = "remote")]
#[must_use]
fn classify_sync_status(connected: bool, synced: bool, owns_session: bool) -> CollabSyncStatus {
    if !connected {
        CollabSyncStatus::Disconnected
    } else if !synced {
        CollabSyncStatus::Syncing
    } else if owns_session {
        CollabSyncStatus::Hosting
    } else {
        CollabSyncStatus::ConnectedToPeer
    }
}

// ── Orbit camera ──────────────────────────────────────────────────────────────

/// Whether the viewport renders with a perspective or orthographic projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionMode {
    Perspective,
    Orthographic,
}

/// Canonical named-view presets, matching Blender's numpad conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewPreset {
    /// Numpad 1 — front (+Z → looking toward -Z, Y-up).
    Front,
    /// Ctrl+Numpad 1 — back (-Z → looking toward +Z).
    Back,
    /// Numpad 3 — right side (+X → looking toward -X).
    Right,
    /// Ctrl+Numpad 3 — left side (-X → looking toward +X).
    Left,
    /// Numpad 7 — top (+Y looking down).
    Top,
    /// Ctrl+Numpad 7 — bottom (-Y looking up).
    Bottom,
}

#[derive(Debug, Clone)]
struct OrbitCamera {
    /// World-space point the camera orbits around.
    target: Vec3,
    /// Horizontal rotation angle in radians.
    yaw: f32,
    /// Vertical elevation angle in radians; clamped to ±89° for free orbit,
    /// set to exact ±90° for named top/bottom views.
    pitch: f32,
    /// Distance from target.
    radius: f32,
    /// Current projection mode (perspective or orthographic).
    projection_mode: ProjectionMode,
    /// Orthographic view half-height in world units (vertical extent visible).
    /// Kept approximately in sync with `radius` when toggling projection.
    ortho_scale: f32,
    /// Up-vector hint passed to `look_at_rh`.
    /// Normally `Vec3::Y`; set to `-Vec3::Z` for top/bottom views where the
    /// look direction is parallel to Y and the standard hint would degenerate.
    up_hint: Vec3,
    /// The named view currently snapped to, or `None` for a free-orbit
    /// (perspective) view. Set by `set_view_preset`; cleared on free orbit.
    current_view: Option<ViewPreset>,
}

impl OrbitCamera {
    fn default_framing() -> Self {
        let radius = 5.0_f32;
        Self {
            target: Vec3::ZERO,
            yaw: std::f32::consts::FRAC_PI_4, // 45°
            pitch: std::f32::consts::FRAC_PI_4, // 45° (top-right 3/4 view)
            radius,
            projection_mode: ProjectionMode::Perspective,
            ortho_scale: Self::radius_to_ortho_scale(radius),
            up_hint: Vec3::Y,
            current_view: None,
        }
    }

    /// Converts an orbit radius to an equivalent orthographic view scale so
    /// the scene occupies a similar area when switching projection modes.
    fn radius_to_ortho_scale(radius: f32) -> f32 {
        // fov_y = 45°, so half-height at `radius` = radius * tan(22.5°)
        radius * 2.0 * (22.5_f32.to_radians()).tan()
    }

    fn eye(&self) -> Vec3 {
        self.target
            + Vec3::new(
                self.radius * self.yaw.cos() * self.pitch.cos(),
                self.radius * self.pitch.sin(),
                self.radius * self.yaw.sin() * self.pitch.cos(),
            )
    }

    fn to_camera_desc(&self, aspect: f32) -> RenderCameraDesc {
        let eye = self.eye();
        // Use self.up_hint so top/bottom views don't degenerate when the look
        // direction is parallel to the default Vec3::Y hint.
        let view = glam::camera::rh::view::look_at_mat4(eye, self.target, self.up_hint);
        let near = (self.radius * 0.005).max(0.001);
        let far = self.radius * 12.0 + 50.0;

        let (projection, fov_h) = match self.projection_mode {
            ProjectionMode::Perspective => {
                let fov_y = 45_f32.to_radians();
                let proj = glam::camera::rh::proj::directx::perspective(fov_y, aspect.max(0.01), near, far);
                let fh = 2.0 * ((fov_y * 0.5).tan() * aspect).atan();
                (proj, fh)
            }
            ProjectionMode::Orthographic => {
                let half_h = (self.ortho_scale * 0.5).max(0.0001);
                let half_w = half_h * aspect.max(0.01);
                // Extend clip range to both sides of the target so prims behind
                // the pivot aren't clipped in orthographic views.
                let proj = glam::camera::rh::proj::directx::orthographic(-half_w, half_w, -half_h, half_h, -far, far);
                let fh = 2.0 * ((half_w / far.max(0.001)).atan());
                (proj, fh)
            }
        };

        RenderCameraDesc {
            prim_path: "__orbit__".to_owned(),
            view,
            projection,
            clip_near: near,
            clip_far: far,
            fov_horizontal: fov_h,
            exposure_ev: 0.0,
        }
    }

    fn orbit(&mut self, delta_yaw: f32, delta_pitch: f32) {
        // Top/bottom presets sit exactly on the yaw singularity (pitch = ±90°)
        // where the eye formula's yaw term collapses and the azimuth is instead
        // carried by `up_hint = -Z`. The free-orbit chart uses `up_hint = Y`, so
        // naively restoring Y while keeping the preset's `yaw = 0` would snap the
        // view 90° about the vertical axis before the tilt is applied. Re-express
        // the pole orientation in the Y-up chart first so the view tilts
        // continuously — matching Blender's quaternion-based numpad orbit.
        match self.current_view {
            Some(ViewPreset::Top) => {
                self.yaw = std::f32::consts::FRAC_PI_2;
                self.pitch = std::f32::consts::FRAC_PI_2;
            }
            Some(ViewPreset::Bottom) => {
                self.yaw = -std::f32::consts::FRAC_PI_2;
                self.pitch = -std::f32::consts::FRAC_PI_2;
            }
            _ => {}
        }
        self.yaw += delta_yaw;
        self.pitch =
            (self.pitch + delta_pitch).clamp(-89.0_f32.to_radians(), 89.0_f32.to_radians());
        // Restore the standard up hint — free orbit always uses Vec3::Y.
        self.up_hint = Vec3::Y;
        // Free orbit breaks out of any named view.
        self.current_view = None;
    }

    fn pan(&mut self, delta_right: f32, delta_up: f32) {
        let eye = self.eye();
        let forward = (self.target - eye).normalize_or_zero();
        // Build the screen basis from the same up hint the view matrix uses
        // (see `to_camera_desc`). In top/bottom views the look direction is
        // parallel to `Vec3::Y`, so a Y-based right axis degenerates to zero and
        // its float residual points along the wrong axes — making pan feel
        // flipped/scrambled. `up_hint` is `-Z` for those views, keeping the pan
        // axes aligned with what's on screen; it is `Y` for every other view, so
        // their behaviour is unchanged.
        let right = forward.cross(self.up_hint).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();
        self.target += right * delta_right + up * delta_up;
    }

    fn dolly(&mut self, factor: f32) {
        self.radius = (self.radius * factor).max(0.001);
        // Keep ortho_scale in sync so toggling projection stays consistent.
        self.ortho_scale = (self.ortho_scale * factor).max(0.0001);
    }

    fn frame_aabb(&mut self, aabb_min: Vec3, aabb_max: Vec3) {
        self.target = (aabb_min + aabb_max) * 0.5;
        let diagonal = (aabb_max - aabb_min).length().max(0.001);
        self.radius = diagonal * 1.8;
        self.yaw = std::f32::consts::FRAC_PI_4;
        self.pitch = std::f32::consts::FRAC_PI_4; // 45° (top-right 3/4 view)
        self.projection_mode = ProjectionMode::Perspective;
        self.ortho_scale = Self::radius_to_ortho_scale(self.radius);
        self.up_hint = Vec3::Y;
        self.current_view = None;
    }

    /// Seeds the orbit params from an explicit eye/target pair so a hand-off
    /// from an authored scene camera to free orbit doesn't make the view jump.
    /// Inverse of [`eye`](Self::eye): recovers `yaw`/`pitch`/`radius` from the
    /// eye position relative to `target`.
    fn set_from_eye_target(&mut self, eye: Vec3, target: Vec3) {
        let dir = eye - target;
        let radius = dir.length().max(0.001);
        self.target = target;
        self.radius = radius;
        self.yaw = dir.z.atan2(dir.x);
        self.pitch = (dir.y / radius).clamp(-1.0, 1.0).asin();
        self.projection_mode = ProjectionMode::Perspective;
        self.ortho_scale = Self::radius_to_ortho_scale(radius);
        self.up_hint = Vec3::Y;
        self.current_view = None;
    }

    /// Sets yaw/pitch for a named canonical view and switches to orthographic.
    fn set_view_preset(&mut self, preset: ViewPreset) {
        // Sync ortho_scale to current radius before switching.
        if self.projection_mode == ProjectionMode::Perspective {
            self.ortho_scale = Self::radius_to_ortho_scale(self.radius);
        }
        self.projection_mode = ProjectionMode::Orthographic;
        self.current_view = Some(preset);
        match preset {
            ViewPreset::Front => {
                self.yaw = std::f32::consts::FRAC_PI_2;
                self.pitch = 0.0;
                self.up_hint = Vec3::Y;
            }
            ViewPreset::Back => {
                self.yaw = -std::f32::consts::FRAC_PI_2;
                self.pitch = 0.0;
                self.up_hint = Vec3::Y;
            }
            ViewPreset::Right => {
                self.yaw = 0.0;
                self.pitch = 0.0;
                self.up_hint = Vec3::Y;
            }
            ViewPreset::Left => {
                self.yaw = std::f32::consts::PI;
                self.pitch = 0.0;
                self.up_hint = Vec3::Y;
            }
            // Top/bottom look along ±Y, so Vec3::Y would degenerate in look_at_rh.
            // Use -Vec3::Z as the up hint: world -Z (scene "front") appears as
            // screen-up in a Y-up top-down view, matching standard DCC convention.
            ViewPreset::Top => {
                self.yaw = 0.0;
                self.pitch = std::f32::consts::FRAC_PI_2; // exactly 90°
                self.up_hint = -Vec3::Z;
            }
            ViewPreset::Bottom => {
                self.yaw = 0.0;
                self.pitch = -std::f32::consts::FRAC_PI_2; // exactly -90°
                self.up_hint = -Vec3::Z;
            }
        }
    }

    /// Toggles between perspective and orthographic projection, keeping the
    /// visible scene area consistent.
    fn toggle_projection(&mut self) {
        match self.projection_mode {
            ProjectionMode::Perspective => {
                self.ortho_scale = Self::radius_to_ortho_scale(self.radius);
                self.projection_mode = ProjectionMode::Orthographic;
            }
            ProjectionMode::Orthographic => {
                self.projection_mode = ProjectionMode::Perspective;
            }
        }
    }
}

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum SessionError {
    #[cfg(feature = "remote")]
    #[error("collaboration application error: {0}")]
    Collaboration(String),
    #[error("stage error: {0}")]
    Stage(#[from] StageError),
    #[error("hydration error: {0}")]
    Hydration(#[from] HydrationError),
}

// ── Open-load metrics (feature = "metrics") ───────────────────────────────────

/// Phase-by-phase timings captured during [`StudioSession::open_with_gpu`].
///
/// Written to the system temp directory as `idtx-metrics-<stage>-<timestamp_ms>.json` once
/// [`StudioSession::finalize_open_metrics`] is called after the first rendered
/// frame. Only compiled when the `metrics` Cargo feature is enabled.
#[cfg(feature = "metrics")]
#[derive(Debug)]
pub struct OpenLoadMetrics {
    pub stage_name: String,
    /// Wall time for `StageService::open` (USD C++ stage open + topology traversal).
    pub usd_open_ms: f64,
    /// Wall time for `hydrator.apply_topology_delta` (ECS entity spawn).
    pub ecs_topology_ms: f64,
    /// Wall time for `service.project_viewport` (full attribute extraction).
    pub usd_viewport_ms: f64,
    /// Wall time for `hydrator.apply_viewport_projection` (ECS component writes).
    pub ecs_viewport_ms: f64,
    /// Wall time for `Renderer::from_device_with_reader` (GPU pipeline compilation).
    pub pipeline_compile_ms: f64,
    /// Wall time of the parallel CPU decode phase in `prefetch_parallel` (scoped threads).
    pub texture_decode_ms: f64,
    /// Wall time of the serial GPU upload phase in `prefetch_parallel`.
    pub texture_upload_ms: f64,
    /// Summed CPU time spent on file reads across all textures (diagnostic).
    pub texture_file_read_ms: f64,
    /// Summed CPU time spent on JPEG/PNG decode across all textures (diagnostic).
    pub texture_image_decode_ms: f64,
    /// Summed CPU time spent on into_rgba8() conversion across all textures (diagnostic).
    pub texture_rgba_convert_ms: f64,
    /// Summed CPU time spent on resolution-cap box downscale across all textures (diagnostic).
    pub texture_cap_resize_ms: f64,
    /// Summed CPU time spent on mip chain generation across all textures (diagnostic).
    pub texture_mip_chain_ms: f64,
    /// Wall time of `render_one_frame` (first GPU submission + camera init).
    pub first_render_ms: f64,
    /// Total wall time from start of `open_with_gpu` through end of first render.
    pub total_ms: f64,
    pub prim_count: usize,
    pub mesh_count: usize,
    pub texture_count: usize,
}

#[cfg(feature = "metrics")]
impl OpenLoadMetrics {
    fn write_json(&self) -> std::io::Result<()> {
        use std::io::Write;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let path =
            std::env::temp_dir().join(format!("idtx-metrics-{}-{}.json", self.stage_name, ts));
        let mut f = std::fs::File::create(&path)?;
        write!(
            f,
            r#"{{
  "schema_version": 1,
  "workspace": "idtx-studio",
  "stage": "{}",
  "phases_ms": {{
    "usd_open": {:.2},
    "ecs_hydration_topology": {:.2},
    "usd_viewport_project": {:.2},
    "ecs_hydration_viewport": {:.2},
    "pipeline_compilation": {:.2},
    "texture_prefetch_decode": {:.2},
    "texture_prefetch_upload": {:.2},
    "first_render_frame": {:.2},
    "total_open_to_first_frame": {:.2}
  }},
  "texture_decode_breakdown_ms": {{
    "file_read": {:.2},
    "image_decode": {:.2},
    "rgba_convert": {:.2},
    "cap_resize": {:.2},
    "mip_chain": {:.2}
  }},
  "counters": {{
    "prim_count": {},
    "mesh_count": {},
    "texture_count": {}
  }}
}}"#,
            self.stage_name,
            self.usd_open_ms,
            self.ecs_topology_ms,
            self.usd_viewport_ms,
            self.ecs_viewport_ms,
            self.pipeline_compile_ms,
            self.texture_decode_ms,
            self.texture_upload_ms,
            self.first_render_ms,
            self.total_ms,
            self.texture_file_read_ms,
            self.texture_image_decode_ms,
            self.texture_rgba_convert_ms,
            self.texture_cap_resize_ms,
            self.texture_mip_chain_ms,
            self.prim_count,
            self.mesh_count,
            self.texture_count,
        )?;
        eprintln!("[idtx-metrics] written to {}", path.display());
        Ok(())
    }
}

/// Per-tick collaboration performance counters.
///
/// Reset each tick; accumulated via instrumentation in `flush_outbound_collab`
/// and `apply_inbound_collab`. Only present when both `metrics` and `remote`
/// features are enabled.
#[cfg(all(feature = "metrics", feature = "remote"))]
#[derive(Debug, Default, Clone)]
pub struct CollabTickMetrics {
    /// Number of prim transforms enqueued for outbound sync in this tick
    /// (after echo-guard filtering).
    pub outbound_edits: u64,
    /// Number of prim transforms received and applied from remote peers.
    pub inbound_edits: u64,
    /// Of `inbound_edits`, those delivered as a decomposed `SeparateTransform`
    /// (the server's preferred form).
    pub inbound_separate: u64,
    /// Of `inbound_edits`, those delivered as a full `Matrix4dTransform`.
    pub inbound_matrix: u64,
    /// Acks rejecting an outbound update as `stale` (a peer edited the prim
    /// after this client's base — newer state is already arriving).
    pub stale_acks: u64,
    /// Acks rejecting an outbound update for any other reason
    /// (`invalid_base` / `apply_failed` / `queue_full` / `session_closing`),
    /// plus out-of-band server `Error` frames.
    pub rejected_acks: u64,
    /// Microseconds spent inside `apply_inbound_collab` (decompose + author).
    pub inbound_apply_us: u64,
}

// ── Session ───────────────────────────────────────────────────────────────────

/// Which camera the desktop frame path renders through (when no XR preview is
/// active).
///
/// The default [`Orbit`](Self::Orbit) keeps the synthetic orbit/viewport
/// camera the editor surfaces always use — and the runtime's fallback when a
/// stage has no local-player camera. The runtime opts into
/// [`ScenePath`](Self::ScenePath) to render through an authored camera (the
/// local player's), whose pose is driven each frame by its attached behavior.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RenderCameraSource {
    /// Synthetic orbit camera (editor default + runtime fallback).
    #[default]
    Orbit,
    /// Render through the authored camera at this USD prim path, recomputing
    /// its descriptor from the live ECS `WorldTransform` each frame.
    ScenePath(String),
}

/// The portable *config* half of an `IdtxBehaviorAPI` behavior — the
/// manifest attributes carried over when a weaker-layer (or detached) behavior
/// is **overwritten** into the current edit layer.
///
/// Read with [`StudioSession::read_composed_behavior_config`] and re-authored
/// with [`StudioSession::apply_behavior_config`]. Deliberately excludes
/// `module`/`source`/`uuid`: an overwrite regenerates a fresh source file with
/// a new uuid, so only the manifest config (kind, scope, enable/authority
/// flags, capabilities, inputs, version, ship, memory cap) survives. Every
/// field is optional/empty so a sparse behavior copies only what it authored.
#[cfg(feature = "authoring")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BehaviorConfigSnapshot {
    pub kind: Option<String>,
    pub scope: Option<String>,
    pub enabled: Option<bool>,
    pub server_authoritative: Option<bool>,
    pub capabilities: Vec<String>,
    pub inputs: Vec<String>,
    pub version: Option<String>,
    pub ship: Option<bool>,
    pub memory_mb: Option<i64>,
}

#[derive(Debug)]
#[cfg(all(feature = "remote", feature = "authoring"))]
struct CollabPersistence {
    observed_root: Option<u64>,
    non_transform: Option<String>,
    transform_opinions: std::collections::BTreeMap<(String, String), String>,
    local_revision: u64,
    local_only_dirty: bool,
    committing: Option<CollabCommitPending>,
}

#[cfg(all(feature = "remote", feature = "authoring"))]
#[derive(Debug)]
struct CollabCommitPending {
    receiver: std::sync::mpsc::Receiver<Result<u64, String>>,
    local_revision: u64,
    intent_revision: u64,
}

/// One-tab controller that wires a [`StageService`], [`SceneHydrator`], and
/// offscreen [`Renderer`] together. Call [`StudioSession::tick`] from the frame
/// loop to apply incremental ECS reprojections, then render the offscreen view.
#[derive(Debug)]
pub struct StudioSession {
    id: SessionId,
    source_path: PathBuf,
    /// Origin of the stage — local file or remote URL. Governs whether the
    /// shell offers in-place "Save" (local) or only "Save As…" (remote).
    source: SessionSource,
    service: StageService,
    hydrator: SceneHydrator,
    renderer: Renderer,
    frame_target: FrameTarget,
    viewport_pixels: [u32; 2],
    orbit_camera: OrbitCamera,
    /// True once the camera has been positioned to frame the initial scene AABB.
    camera_initialized: bool,
    /// Which camera `render_one_frame` renders through when no XR preview is
    /// active. [`RenderCameraSource::Orbit`] (the default) uses the synthetic
    /// orbit camera — the editor surfaces (inspector/poser/conductor) never
    /// change it. The runtime opts into
    /// [`RenderCameraSource::ScenePath`] via [`Self::use_scene_camera`] to
    /// render through an authored (local-player) camera.
    render_camera: RenderCameraSource,
    /// Currently selected prim path, if any.
    selected_path: Option<String>,
    /// Mirrors the renderer's editor-overlay toggle so overlays built session-side
    /// (e.g. the collision wireframe) also hide when a preview player disables
    /// editor chrome at runtime. Default `true`; see [`Self::set_editor_overlays_enabled`].
    editor_overlays_enabled: bool,
    /// Diagnostic messages from the most-recent viewport projection.
    diagnostics: Vec<String>,
    /// Root coordinate-system correction: maps USD world space → render world space.
    /// Encodes both the up-axis rotation (Y-up vs Z-up) and the units-to-metres scale.
    projection_basis: Mat4,
    /// Inverse of `projection_basis`; used to transform pick rays back to USD world space.
    projection_basis_inv: Mat4,
    /// Play-space recenter offset (stage-space ground vector, y = 0): the amount
    /// the pawn origin is re-based by so the body tracks the headset. Updated
    /// each XR tick by [`recenter_step`] from the HMD ground position; fed to the
    /// behavior host (so `xr_player_move` bakes it into `/World/Player`) and
    /// subtracted from the eye + fed child poses so the view and hydrated
    /// camera/controllers stay fixed. `ZERO` outside XR / before the first snap.
    xr_recenter: glam::Vec3,
    /// Configured render world unit scale (default: 1 unit = 1 metre).
    world_unit_scale: WorldUnitScale,
    /// Root-layer shadow mode (`Off`/`Hard`/`Soft`), read once from
    /// `StageMetadata` at open and updated live by [`Self::set_stage_shadow_mode`].
    /// The stage-authored half of the shadow enable decision; maps to a render
    /// tier, clamped by the hardware cap and gated by the product capability.
    stage_shadow_mode: idtx_usd::ShadowMode,
    /// Live stage-behavior run-condition, read once from [`StageMetadata`] at
    /// open and updated by [`Self::set_stage_behavior_run_scope`] /
    /// [`Self::ensure_stage_behavior_prim`]. Authoring `customLayerData` does not
    /// refresh the cached `StageMetadata`, so (like [`Self::stage_shadow_mode`])
    /// this live copy is the UI + run-gate source of truth after an in-session
    /// edit.
    stage_behavior_run_scope: idtx_usd::StageBehaviorRunScope,
    /// Product-level shadow capability, set by the shell from
    /// `ProductProfile::shadows_enabled` (default `true` until the shell clamps
    /// it — `false` for Inspector/Poser). The product half of the decision.
    shadow_capability: bool,
    /// Target-hardware ceiling on shadow quality (Quest 2/3/Pro clamp expensive
    /// tiers). `Soft` on desktop; XR callers lower it. The resolved per-frame
    /// tier is `min(desktop_default, this)` gated by the two flags above.
    shadow_tier_cap: idtx_render::ShadowTier,
    /// Monotonically increasing counter bumped each time a topology change is
    /// applied.  Callers can compare against a cached value to detect when the
    /// prim tree needs to be rebuilt.
    topology_generation: u64,
    /// Monotonically increasing counter bumped each time a visibility-only
    /// change is applied.  Callers can compare against a cached value to detect
    /// when prim visibility needs to be refreshed in the scene tree.
    visibility_generation: u64,
    /// Monotonically increasing counter bumped on every successful `tick()` call
    /// that processed at least one USD change (topology or value-only).  Use
    /// this to invalidate caches that depend on attribute values, such as the
    /// inspector property cache.
    change_generation: u64,
    /// Diagnostic counter (always-on): how many times `tick()` has forced a full
    /// snapshot re-extract because no patch path was taken. Used only to
    /// rate-limit the `extract-cause` log that attributes the Quest full-extract
    /// rate (`extracted_this_frame_ratio`) to a specific per-frame cause.
    tick_force_extract_count: u64,
    /// Last session-layer visibility authored per prim path, so a behavior that
    /// re-asserts the SAME visibility every frame (e.g. a gaze cursor shown while
    /// aiming at a target — see `left_aim_sync`) is deduped to a no-op. A
    /// VISIBILITY change otherwise drops the whole frame off the transform-only
    /// fast path into a full re-extract AND sets `snapshot_dirty` directly, so
    /// re-authoring an unchanged value was a per-frame full-extract driver on XR.
    /// Cleared on topology change (the prim may be re-hydrated).
    session_visibility_authored: std::collections::HashMap<String, bool>,
    /// Cached render snapshot from the previous `render_one_frame` call.
    /// `None` before the first successful extraction.
    last_snapshot: Option<RenderWorldSnapshot>,
    /// Set to `true` whenever the ECS world changes (USD change batch, coordinate
    /// system update).  Cleared after re-extraction in `render_one_frame`.
    /// When `false` and `last_snapshot` is `Some`, extraction is skipped.
    snapshot_dirty: bool,
    /// Pending world-matrix patches from a transform-only animation tick.
    ///
    /// Set instead of `snapshot_dirty` when only `xformOp` tracks advanced.
    /// `render_one_frame` calls `patch_world_matrices` and clears this field,
    /// avoiding a full O(N-meshes) re-extraction for pure transform animation.
    /// Cleared (set to `None`) whenever a full `snapshot_dirty` extract runs.
    snapshot_transform_patches: Option<Vec<(String, [f32; 16])>>,
    /// The single WASM behavior host for this session (unification). It
    /// drives both authored runtime behaviors and the XR-preview
    /// camera/controller sync behaviors, ticked once per frame by
    /// [`Self::tick_behaviors`]. Built with [`Limits::conductor`]; the runtime
    /// player rebuilds it with [`Limits::runtime`] via [`Self::set_behavior_limits`].
    behaviors: BehaviorHost,
    authored_behaviors_enabled: bool,
    behavior_reconcile_pending: bool,
    pending_level_cancellation: bool,
    /// Per-prim highlights a behavior requested via the `overlay` channel this
    /// frame, drained from the host in [`Self::tick_behaviors`] and rendered by
    /// [`Self::render_one_frame`]: a wireframe box ([`HighlightStyle::Box`]), a
    /// surface tint ([`HighlightStyle::Tint`]), or a silhouette outline
    /// ([`HighlightStyle::Outline`]). Transient — rebuilt each behavior tick.
    behavior_highlights: Vec<HighlightEntry>,
    /// World-space overlay lines a behavior requested via the `overlay` channel
    /// this frame (`draw_line` / `draw_arc` commands resolved by
    /// [`resolve_overlay_lines`]) — the teleport pointer arc + landing marker.
    /// Drawn through the post-tonemap overlay line pass on both the desktop and
    /// the XR/visionOS eye paths. Transient — rebuilt each behavior tick.
    behavior_overlay_lines: Vec<idtx_render::OverlayLine>,
    presentation_overlay_lines: Vec<idtx_render::OverlayLine>,
    /// The most recent level-transition request a `level_control` behavior posted
    /// this frame, drained from the host in
    /// [`Self::tick_behaviors`]. Surfaced — not acted on — here: the runtime
    /// driver polls it via [`Self::take_pending_level_transition`] and performs
    /// the actual session swap. Not `authoring`-gated: the shipped runtime
    /// reads it. If several requests arrive in one frame the most recent wins.
    pending_level_transition: Option<idtx_behavior::LevelTransitionRequest>,
    /// Behavior transport state (Play/Stop). A dedicated field, independent of
    /// the animation clock (`StagePlayback.playing`); defaults to
    /// [`PlayState::Authoring`]. The packaged runtime player sets it `Playing`.
    play_state: PlayState,
    /// Whether the stage's **root layer** holds unsaved authoring edits. Set in
    /// [`Self::tick`] when a drained change batch touched the root layer while
    /// in [`PlayState::Authoring`]; cleared by [`Self::mark_stage_saved`] after
    /// a successful save. Session-layer edits (visibility overrides, variant
    /// selection, runtime physics write-back) never set it. Drives the tab's
    /// unsaved-changes hint and the save-on-close prompt. Authoring-only — the
    /// packaged runtime neither authors the root layer nor reads this flag.
    #[cfg(feature = "authoring")]
    stage_dirty: bool,
    /// Root-layer content signature captured by [`Self::mark_stage_saved`] at the
    /// last successful persist. Because `SdfLayer::IsDirty` stays `true` after an
    /// `Export`-based save (in-place loose Save *and* Save As both go through
    /// `export_root_layer`, which never clears the bit), the dirty probe in
    /// [`Self::tick`] confirms a set dirty bit against this baseline: equal hash ⇒
    /// no unsaved root-layer edits since the save. `None` until the first save —
    /// then a set dirty bit means genuine unsaved edits.
    #[cfg(feature = "authoring")]
    saved_root_hash: Option<u64>,
    /// Per-frame runtime input accumulator. The shell/runtime reports
    /// device events into it each frame; [`Self::tick_behaviors`] feeds its
    /// [`FrameInput::raw`] snapshot into the behavior host before `run_frame`
    /// and rolls it with [`FrameInput::end_frame`] after. The host resolves
    /// named actions against the router installed via
    /// [`Self::set_behavior_input_map`].
    frame_input: FrameInput,
    /// Priority-ordered input mapping-context stack. The runtime
    /// player layers named [`InputMap`]s here (a transient "menu"/"vehicle"
    /// context can consume actions from the gameplay context beneath it); each
    /// mutation via [`Self::push_mapping_context`] / [`Self::remove_mapping_context`]
    /// / [`Self::reprioritise_mapping_context`] / [`Self::clear_mapping_contexts`]
    /// re-flattens the stack and hot-reloads the active bindings into the host
    /// router. Empty by default — with no context pushed the map installed via
    /// [`Self::set_behavior_input_map`] is left untouched. Behavior execution is
    /// runtime-only, so this is never driven from the editor shell.
    mapping_contexts: MappingContextStack,
    /// Set when a transform-only change also moved a *skinned* subtree (e.g. the
    /// rigidly re-posed skinned XR controller prefab), so only the GPU joint
    /// palette needs refreshing. `render_one_frame` calls
    /// [`RenderWorldSnapshot::patch_joint_matrices`] from the ECS poses and clears
    /// this, avoiding a whole-stage re-extract and keeping `snapshot.serial`
    /// stable (which in turn keeps the stereo eye0 buffers cached). Cleared
    /// whenever a full `snapshot_dirty` extract runs.
    snapshot_skeleton_dirty: bool,
    /// Set when a value-only widget edit (`idtx:widget:*`, e.g. an `OpenExec`
    /// compute writeback into a panel's text/value) changed a panel whose prim
    /// already exists in the warm snapshot, and nothing heavier
    /// (geometry/material/light/visibility) changed this tick. `render_one_frame`
    /// calls [`RenderWorldSnapshot::patch_widget_panels`] to re-read the affected
    /// panels' `WidgetPanelComp` in place (the widget pass re-rasterizes only the
    /// panels whose `content_hash` moved) and clears this — avoiding a whole-stage
    /// re-extract per datasource interval and keeping `snapshot.serial` stable.
    /// Cleared whenever a full `snapshot_dirty` extract runs.
    snapshot_widget_dirty: bool,
    /// Set when a value-only *geometry* edit on a parametric gprim (Cube `size`,
    /// Sphere `radius`, Cylinder/Cone/Capsule `height`/`axis`, Plane
    /// `width`/`length` — e.g. an `OpenExec` compute writeback onto a cube `size`)
    /// changed a prim already present in the warm snapshot, with nothing heavier
    /// (transform / material / light / visibility / skeleton / topology) in the
    /// batch. `prepare_presentation` calls
    /// [`RenderWorldSnapshot::patch_mesh_geometry`] to re-tessellate ONLY
    /// `snapshot_geometry_paths` in place and clears this — the changed prim's
    /// per-mesh `GpuMeshDesc::serial` moves so the renderer re-uploads just that
    /// mesh and re-bakes just its merge group, while `snapshot.serial` stays stable
    /// so unrelated static-merge / instance / material / AABB caches survive.
    /// Poly `MeshComp` prims (`points`/topology edits) are deliberately excluded
    /// and keep the full re-extract path. Cleared whenever a full `snapshot_dirty`
    /// extract runs.
    snapshot_geometry_dirty: bool,
    /// Prim paths whose geometry the pending `patch_mesh_geometry` must refresh.
    /// Drained (taken) when the patch is applied; non-empty only while
    /// `snapshot_geometry_dirty` is set.
    snapshot_geometry_paths: Vec<String>,
    /// Pending scoped render-snapshot splice from a localized topology change
    /// (runtime spawn fast path). Holds the newly-spawned / retopologised prim
    /// IDs (incl. any resolved point-instancer prototype roots). When `Some` and
    /// a warm `last_snapshot` exists, `prepare_presentation` calls
    /// [`RenderWorldSnapshot::splice_subtree`] to insert just these prims into the
    /// warm snapshot **without** bumping `snapshot.serial` — so the static
    /// draw-merge / instance / AABB caches survive (the spawn analog of the
    /// transform / geometry / widget fast paths). Falls back to a full extract
    /// when the subtree contains skinning / instancing / a light, or no warm
    /// snapshot exists. Set only for a purely-additive localized topology delta;
    /// unioned across drains, then cleared by presentation or full invalidation.
    snapshot_topology_splice: Option<HashSet<idtx_usd::PrimId>>,
    /// Active `OpenXR` preview, when the user has started "Play in `OpenXR`".
    /// `None` outside a preview; drives the camera from the HMD each frame.
    #[cfg(feature = "xr-preview")]
    xr_preview: Option<xr_preview::XrPreviewSession>,
    /// Fallback capsule meshes for the left/right XR controllers.
    /// Built once on the first frame when an XR preview is active. Legacy
    /// mesh-bake path — gated behind `xr-bake` (off by default).
    #[cfg(feature = "xr-bake")]
    xr_controller_mesh: Option<xr_controller_mesh::ControllerMeshCache>,
    /// Template bone/joint meshes for hand-skeleton visualisation.
    /// Built once on the first frame when hand-tracking data arrives.
    #[cfg(feature = "xr-preview")]
    xr_hand_mesh: Option<xr_hand_mesh::HandMeshCache>,
    /// Controls whether controller capsules or hand-skeleton geometry is shown.
    #[cfg(feature = "xr-preview")]
    xr_hand_viz: xr_hand_mesh::XrHandVisualization,
    /// Per-hand opt-in for the Phase-0 hydrated-controller spike (see
    /// `docs/xr-controller-hydration-plan.md`); `[left, right]`. When a hand is
    /// hydrated, its controller renders through the normal USD hydrate/extract
    /// path — a session `over` on the existing `/XR/Controllers/{hand}` proxy
    /// flips it visible and carries the per-frame `OpenXR` pose — and the legacy
    /// bake injection is suppressed for that hand so the two paths never draw the
    /// same controller twice.
    #[cfg(all(feature = "xr-preview", feature = "authoring"))]
    xr_hydrated_controllers: [bool; 2],
    /// Single-authority latch: whether an authored WASM behavior owns each XR
    /// pose prim, so the session's own hydrated pose write must stand down for
    /// it. Indices: `[0]`/`[1]` = left/right controller (grip), `[2]`/`[3]` =
    /// left/right pointer (aim). Recomputed by [`attach_authored_behaviors`]
    /// each time behaviors are (re)attached; see
    /// [`author_hydrated_controller_poses`].
    #[cfg(all(feature = "xr-preview", feature = "authoring"))]
    xr_pose_behavior_owned: [bool; 4],
    /// Last-authored visibility of each XR pose prim, so the per-frame
    /// hand-vs-controller gating authors a visibility `over` only on a *change*
    /// (never every frame). Indices mirror [`xr_pose_behavior_owned`]: `[0]`/`[1]`
    /// = left/right controller, `[2]`/`[3]` = left/right pointer. All start visible
    /// (`true`); [`apply_xr_hand_controller_visibility`] hides a hand's controller +
    /// pointer when that hand is actively hand-tracked (per-hand, live-pose gated).
    #[cfg(all(feature = "xr-preview", feature = "authoring"))]
    xr_pose_prim_visible: [bool; 4],
    /// One-shot latch for the session-authored-`over` console dump: the first
    /// time a hydrated-controller pose is authored we print the Rust-tracked
    /// session state (paths + visibility + pose matrices) once, so the authored
    /// `over`s can be inspected without a C++ FFI into the composed stage. See
    /// `dump_session_overrides` and `docs/xr-controller-hydration-plan.md` §5.
    #[cfg(all(feature = "xr-preview", feature = "authoring"))]
    xr_session_dumped: bool,
    /// Stage C stereo render resources (per-eye targets + wrapped swapchain
    /// textures), created when a real graphics preview starts (D3D12 or Vulkan).
    #[cfg(idtx_xr)]
    xr_gpu: Option<xr_render::XrGpu>,
    /// Apple Vision Pro per-eye render resources (per-eye targets + drawable
    /// blit pipeline), created lazily on the first `visionos_render_frame`.
    #[cfg(all(feature = "visionos", target_vendor = "apple"))]
    visionos_gpu: Option<visionos_render::VisionosGpu>,
    /// The scene-anchor transform placing the stage in front of the viewer,
    /// computed once from the scene AABB + the first frame's head pose (so the
    /// model is room-anchored a comfortable distance ahead rather than sitting at
    /// the raw ARKit world origin at the user's feet).
    #[cfg(all(feature = "visionos", target_vendor = "apple"))]
    visionos_anchor: Option<Mat4>,
    /// Frames seen before locking the room anchor. The first frames' head pose can
    /// be the identity (ARKit tracking not yet settled), which would anchor the
    /// scene along the wrong direction; we head-lock during warmup, then lock a
    /// room anchor from a settled pose.
    #[cfg(all(feature = "visionos", target_vendor = "apple"))]
    visionos_anchor_warmup: u32,
    #[cfg(feature = "metrics")]
    t_open_start: std::time::Instant,
    #[cfg(feature = "metrics")]
    open_metrics: Option<OpenLoadMetrics>,
    /// Wall-clock nanoseconds for the last `render_one_frame()` call (total).
    #[cfg(feature = "metrics")]
    last_render_frame_ns: u64,
    /// Nanoseconds spent in `RenderWorldSnapshot::extract` during the last frame.
    #[cfg(feature = "metrics")]
    last_extract_ns: u64,
    /// Nanoseconds spent in `renderer.render_with_overlay` during the last frame.
    #[cfg(feature = "metrics")]
    last_gpu_render_ns: u64,
    /// Nanoseconds spent on USD animation sampling in the last `tick_animation` call.
    /// Covers `classify_animated_prim_ids` + the `service.sample_*` C++ call +
    /// `hydrator.apply_*` — everything from prim-ID classification to ECS write-back.
    /// This is the primary diagnostic for CpuOpenUsd skinning stalls.
    #[cfg(feature = "metrics")]
    last_anim_sample_ns: u64,
    /// Cached pseudo-root entity from the hydrator. Populated after the first
    /// topology hydration; used to read/write `StagePlayback` / `StageAnimClip`.
    pseudo_root: Option<idtx_scene::bevy_ecs::entity::Entity>,
    /// Runtime physics subsystem, built lazily on the first `tick_physics` after
    /// a `UsdPhysicsScene` prim is detected in the hydrated stage (plan §3).
    /// `None` when there is no scene prim — simulation stays off, as it always
    /// is in the editor (the `physics` feature is runtime-only).
    #[cfg(feature = "physics")]
    physics: Option<physics::PhysicsSubsystem>,
    /// Topology generation last reconciled by physics (or acknowledged for a
    /// proven physics-neutral addition). Reconciliation preserves unchanged
    /// bodies' solver state. `None` until the first build attempt or invalidation.
    #[cfg(feature = "physics")]
    physics_build_gen: Option<u64>,
    /// Physics-control commands (grab suspend/resume) a behavior posted before the
    /// physics subsystem was built. Behaviors run (and can post a grab) on the
    /// first frame, *before* the lazy build in the first `tick_physics`; retaining
    /// them here and applying them right after the build means a first-frame grab
    /// is not silently lost (PR #16 review). Drained every `tick_physics`.
    #[cfg(feature = "physics")]
    pending_physics_control: Vec<PhysicsControlCommand>,
    /// Collider prims touched by *value-only* geometry edits since the last
    /// `tick_physics` (e.g. an `OpenExec`-driven cube `size`). Drives an
    /// incremental per-body reconcile ([`physics::reconcile_dirty`]) next frame
    /// instead of nulling `physics_build_gen` (which would force a whole-stage
    /// `detect_and_build`). Drained — and superseded — whenever a full rebuild
    /// runs. Only populated for prims that actually own a `CollisionComp`, so a
    /// non-collider geometry edit queues nothing and does zero physics work.
    #[cfg(feature = "physics")]
    physics_dirty_prims: Vec<idtx_usd::PrimId>,
    /// `OpenExec` dataflow subsystem — present only when the open stage carries an
    /// IDTX compute graph (≥1 attribute with an authored connection). Owns the
    /// REST poller threads + their response channel. `None` when the stage has no
    /// compute graph, so `tick_exec` is a cheap no-op. See [`exec::ExecSubsystem`].
    #[cfg(feature = "openexec")]
    exec: Option<exec::ExecSubsystem>,
    /// Topology generation the exec program was last (re)built at. Rebuilt on
    /// topology change (the value-key set is fixed at build; a resync may add or
    /// remove compute prims). `None` until the first build attempt.
    #[cfg(feature = "openexec")]
    exec_build_gen: Option<u64>,
    /// Fingerprint of the REST/mock datasource set the live exec poll pool was
    /// built from. On a topology change the pool only needs tearing down and
    /// re-registering (per-source SSRF validation + DNS + reqwest client build —
    /// the work that froze the stage on every runtime spawn) when this set
    /// actually changes. A localized spawn of exec-irrelevant prims leaves it
    /// identical, so the running pollers are kept and only the C++ program is
    /// refreshed. `0` until the first build.
    #[cfg(feature = "openexec")]
    exec_sources_fp: u64,
    /// Whether the current exec program has completed at least one `exec_tick`
    /// since it was (re)built. The idle gate (skip the compute when nothing was
    /// drained and there is no live provider) must NOT fire before this first
    /// tick, or a datasource-free/static graph — or a datasource graph before its
    /// first payload — would never evaluate and its authored defaults / static
    /// computations (including computed REST authorization inputs) would never
    /// reach consumers. Reset to `false` on every rebuild.
    #[cfg(feature = "openexec")]
    exec_did_initial_tick: bool,
    /// Network capability gate shared by the behavior `net` egress import and
    /// stage-authored REST datasources. Deny-by-default
    /// ([`ExecNetworkPolicy::denied`]); the runtime raises it via
    /// [`Self::set_exec_network_policy`] from its `[network]` config so a hostile
    /// stage cannot trigger unapproved HTTP (SSRF). Owned by the `remote` feature
    /// (the network stack); `openexec`'s datasource poller consumes it each rebuild.
    #[cfg(feature = "remote")]
    exec_network_policy: ExecNetworkPolicy,
    /// Credential write destinations, kept through behavior-host replacement.
    auth_scopes: idtx_auth::AuthScopes,
    /// Active collaboration client, when the session is connected to a sync server.
    #[cfg(feature = "remote")]
    collab: Option<idtx_collab::client::CollabClient>,
    #[cfg(all(feature = "remote", feature = "authoring"))]
    collab_preparation: Option<collab_prepare::Preparation>,
    /// Applied remote local matrices for the next notice batch. Suppress only
    /// matching values, not a newer local edit that happens to share the path.
    #[cfg(feature = "remote")]
    pending_remote_transforms: std::collections::HashMap<String, [f64; 16]>,
    /// User-facing collaboration notices (e.g. a server-rejected prim create),
    /// drained by the shell each frame into its status line. Populated in
    /// `apply_inbound_collab` from the collab client's protocol events.
    #[cfg(feature = "remote")]
    collab_notices: Vec<String>,
    /// Server `LayerReloaded` notices drained by the shell each frame. Each names
    /// a file the stage uses that the server reloaded; the shell re-fetches it
    /// (force-refresh), reloads it into the stage, and re-gates on a fresh
    /// snapshot. Populated in `apply_inbound_collab`.
    #[cfg(feature = "remote")]
    pending_layer_reloads: Vec<PendingLayerReload>,
    /// Recoverable pre-replay opinions, never automatically re-authored.
    #[cfg(feature = "remote")]
    collab_recovery: Vec<(String, String)>,
    #[cfg(all(feature = "remote", feature = "authoring"))]
    collab_persistence: Option<CollabPersistence>,
    #[cfg(all(feature = "remote", feature = "authoring"))]
    collab_applying: bool,
    #[cfg(all(feature = "remote", feature = "authoring"))]
    collab_attributes: collab_attributes::AttributeSync,
    /// Per-tick collab performance counters; reset and populated in `tick()`.
    #[cfg(all(feature = "metrics", feature = "remote"))]
    collab_tick_metrics: Option<CollabTickMetrics>,
    /// Rolling XR-frame performance accumulator; populated in `render_xr` and
    /// dumped to JSON on `stop_xr_preview`. Both readers live in the
    /// `xr-preview` impl, hence `allow(dead_code)` for a metrics-only build.
    #[cfg(feature = "metrics")]
    #[allow(dead_code)]
    xr_metrics: xr_metrics::XrMetricsAccumulator,
    /// Wall time of the most recent `author_hydrated_controller_poses` call
    /// (side-channel for `render_xr`'s per-frame timing; the call site is behind
    /// the `authoring` gate so it cannot be timed with a plain local there).
    /// Only read by `render_xr` (pcvr + windows), hence `allow(dead_code)` for
    /// the metrics-without-pcvr build.
    #[cfg(feature = "metrics")]
    #[allow(dead_code)]
    xr_last_author_ns: u64,
    /// Nanoseconds spent in the reprojection `tick()` inside the most recent
    /// `author_hydrated_controller_poses` call (side-channel for `render_xr`'s
    /// per-frame timing, which cannot otherwise see inside the author call).
    /// Only read by `render_xr` (pcvr + windows), hence `allow(dead_code)` for
    /// the metrics-without-pcvr build.
    #[cfg(feature = "metrics")]
    #[allow(dead_code)]
    xr_last_reproject_ns: u64,
}

impl StudioSession {
    /// Opens `path` as a USD stage and performs the initial full hydration.
    ///
    /// The renderer uses the provided `device` and `queue` so that the
    /// offscreen render texture lives on the same wgpu device as the caller's
    /// egui surface renderer.  Pass `viewport` as `[width, height]` in pixels.
    pub fn open_with_gpu(
        path: impl AsRef<Path>,
        device: wgpu::Device,
        queue: wgpu::Queue,
        viewport: [u32; 2],
        texture_config: TextureDecodeConfig,
        world_unit_scale: WorldUnitScale,
    ) -> Result<Self, SessionError> {
        Self::open_with_gpu_and_session_layer(
            path,
            device,
            queue,
            viewport,
            texture_config,
            world_unit_scale,
            "",
        )
    }

    /// Like [`open_with_gpu`](Self::open_with_gpu), but composes
    /// `session_layer_text` (USDA) as the stage's strongest anonymous **session**
    /// layer at open (via [`StageService::open_with_session`]) — content authored
    /// there composes over the packaged root without touching it on disk.
    ///
    /// The packaged XR runtime uses this to inject the runtime-detected controller
    /// `Model` reference nodes onto the rig's already-authored pose Xforms (see
    /// [`xr_controller_prefab::controller_model_over_layer`]), the twin of the
    /// preview path's generated sublayer. Pass `""` for an ordinary no-injection
    /// open (equivalent to [`open_with_gpu`](Self::open_with_gpu)).
    ///
    /// Asset paths inside `session_layer_text` must be **absolute** — the
    /// anonymous session layer has no directory to anchor a relative reference.
    pub fn open_with_gpu_and_session_layer(
        path: impl AsRef<Path>,
        device: wgpu::Device,
        queue: wgpu::Queue,
        viewport: [u32; 2],
        texture_config: TextureDecodeConfig,
        world_unit_scale: WorldUnitScale,
        session_layer_text: &str,
    ) -> Result<Self, SessionError> {
        Self::open_with_gpu_impl(
            path,
            device,
            queue,
            viewport,
            texture_config,
            world_unit_scale,
            session_layer_text,
            /*load_payloads=*/ true,
            /*on_demand_default_level=*/ None,
        )
    }

    /// On-demand level open: opens the assembly with payloads **unloaded**
    /// (`LoadNone`) and, if `default_level` names a level holder
    /// (e.g. `/World/Levels/Hub`), loads+shows only that one via the existing
    /// [`transition_level`](Self::transition_level) path. Non-default levels stay
    /// uncomposed until a later transition — so a large multi-level assembly only
    /// pays to hydrate what is actually shown, instead of eagerly composing every
    /// level payload at open (the `open_with_gpu*` default). Pair with a cook that
    /// keeps levels as loose payloads ([`idtx_usd::stage_loose_closure`]).
    ///
    /// `default_level` of `None` opens the bare assembly (no level composed) for a
    /// caller that drives the first transition itself.
    #[cfg(feature = "authoring")]
    pub fn open_with_gpu_on_demand(
        path: impl AsRef<Path>,
        device: wgpu::Device,
        queue: wgpu::Queue,
        viewport: [u32; 2],
        texture_config: TextureDecodeConfig,
        world_unit_scale: WorldUnitScale,
        session_layer_text: &str,
        default_level: Option<&str>,
    ) -> Result<Self, SessionError> {
        Self::open_with_gpu_impl(
            path,
            device,
            queue,
            viewport,
            texture_config,
            world_unit_scale,
            session_layer_text,
            /*load_payloads=*/ false,
            default_level,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn open_with_gpu_impl(
        path: impl AsRef<Path>,
        device: wgpu::Device,
        queue: wgpu::Queue,
        viewport: [u32; 2],
        texture_config: TextureDecodeConfig,
        world_unit_scale: WorldUnitScale,
        session_layer_text: &str,
        load_payloads: bool,
        on_demand_default_level: Option<&str>,
    ) -> Result<Self, SessionError> {
        #[cfg(feature = "metrics")]
        let t_open_start = std::time::Instant::now();

        let source_path = path.as_ref().to_path_buf();

        #[cfg(feature = "metrics")]
        let t = std::time::Instant::now();
        let mut service = StageService::open_with_session(&source_path, session_layer_text, load_payloads)?;
        #[cfg(feature = "metrics")]
        let usd_open_ms = t.elapsed().as_secs_f64() * 1000.0;

        // Compute the coordinate-system correction from stage root layer metadata.
        let meta = service.metadata();
        let up_axis = UpAxis::from_usd_token(&meta.up_axis);
        let stage_shadow_mode = meta.shadow_mode;
        let stage_behavior_run_scope = meta.stage_behavior_run_scope.unwrap_or_default();
        let projection_basis = pseudo_root_matrix(world_unit_scale, meta.meters_per_unit, up_axis);
        let projection_basis_inv = projection_basis.inverse();

        let topo = service.topology();
        let delta = TopologyDelta {
            upserts: topo.to_vec(),
            removed: Default::default(),
        };
        let mut registry = PopulatorRegistry::new();
        registry.register(std::sync::Arc::new(idtx_scene::HudPopulator));
        registry.register(std::sync::Arc::new(idtx_scene::widget::WidgetPopulator));
        let mut hydrator = SceneHydrator::new(registry);

        #[cfg(feature = "metrics")]
        let t = std::time::Instant::now();
        hydrator.apply_topology_delta(&delta, &service);
        #[cfg(feature = "metrics")]
        let ecs_topology_ms = t.elapsed().as_secs_f64() * 1000.0;

        #[cfg(feature = "metrics")]
        let t = std::time::Instant::now();
        let projection = service.project_viewport()?;
        #[cfg(feature = "metrics")]
        let usd_viewport_ms = t.elapsed().as_secs_f64() * 1000.0;

        let diagnostics = projection.diagnostics.clone();

        // Surface skinning-mode decisions to the console so offline analysis
        // (e.g. comparing frame-metric JSON files) can identify CpuOpenUsd stalls
        // without needing to reproduce the session interactively.
        #[cfg(feature = "metrics")]
        for d in &diagnostics {
            if d.contains("UsdSkel") || d.contains("skinning") || d.contains("fallback") {
                eprintln!("[idtx-usd] skin-diagnostic: {d}");
            }
        }

        #[cfg(feature = "metrics")]
        let t = std::time::Instant::now();
        hydrator.apply_viewport_projection(&projection)?;
        #[cfg(feature = "metrics")]
        let ecs_viewport_ms = t.elapsed().as_secs_f64() * 1000.0;

        // Initialise per-prim VisibilityOverride from authored USD state.
        // Must run after apply_viewport_projection so all entities exist.
        hydrator.initialize_visibility_overrides(&service);

        // Hydrate BehaviorRef components from authored `idtx:behavior:module`
        // attachments so authored runtime behaviors are picked up by
        // `attach_authored_behaviors`. Must run after all entities
        // exist; the BehaviorPopulator cannot do this (no per-attribute read).
        hydrator.hydrate_authored_behaviors(&service);

        // Hydrate PlayerRole components from authored `idtx:playerRole` so the
        // behavior authority gate can see per-pawn ownership. Same
        // rationale as behaviors: an FFI-scan, not a populator.
        hydrator.hydrate_player_roles(&service);

        // Hydrate CollisionComp from the IdtxCollisionAPI (`idtx:collision:*`)
        // schema so picking (collision filter + shape narrow-phase) and the
        // editor collision wireframe can see it. Same rationale: an FFI-scan,
        // not a populator (a HasAPI schema applies to any prim type).
        hydrator.hydrate_collision(&service);
        // Collision bodies (`IdtxCollisionBodyAPI`): the whole-asset interaction
        // grouping marker, probed independently of the collider gate.
        hydrator.hydrate_collision_bodies(&service);
        // Rigid-body grouping marker (UsdPhysicsRigidBodyAPI) so a grab resolves a
        // hit child collider up to its simulated body root (`rigid_body_of`).
        hydrator.hydrate_rigid_bodies(&service);

        // Hydrate HudComp from the IdtxHud (`idtx:hud:*`) concrete-typed schema
        // (camera-HUD overlay). Same FFI-scan rationale as collision: the
        // populator cannot read per-attribute values. Picked up each frame by
        // `collect_hud_elements` for the screen-space HUD pass.
        hydrator.hydrate_hud(&service);
        // Widget panels (2D widgets in 3D): same drain-time scan — the populator
        // cannot read per-attribute values. Extracted into the render snapshot's
        // `widget_panels` and drawn as depth-tested quads in the beauty pass.
        hydrator.hydrate_widgets(&service);

        // Animation: populate per-prim AnimatedAttribute components and attach
        // the stage-level StageAnimClip + StagePlayback to the pseudo-root.
        idtx_scene::animation::populate_all_anim_attributes(hydrator.world_mut(), &service);
        let pseudo_root = hydrator.pseudo_root_entity();
        if let Some(root) = pseudo_root {
            idtx_scene::animation::attach_stage_anim_clip(hydrator.world_mut(), &service, root);
        }

        let vp = [viewport[0].max(1), viewport[1].max(1)];
        let reader: AssetReadFn = Arc::new(move |path: &str| idtx_usd::read_asset_bounded(path, texture_config.max_decode_bytes / 4).ok());

        #[cfg(feature = "metrics")]
        let t = std::time::Instant::now();
        let mut renderer = Renderer::from_device_with_reader(device, queue, reader, texture_config);
        #[cfg(feature = "metrics")]
        let pipeline_compile_ms = t.elapsed().as_secs_f64() * 1000.0;

        // Pre-decode all textures in parallel before the first render so
        // build_material_bg hits the cache instead of blocking on disk I/O.
        let prefetch_aspect = vp[0] as f32 / vp[1] as f32;
        let prefetch_snap =
            RenderWorldSnapshot::extract(hydrator.world_mut(), prefetch_aspect, projection_basis);
        #[cfg(feature = "metrics")]
        let mesh_count = prefetch_snap.meshes.len();
        let (prefetch_count, decode_elapsed, upload_elapsed, tex_timings) =
            renderer.prefetch_textures(&prefetch_snap);
        let _ = (prefetch_count, decode_elapsed, upload_elapsed, tex_timings);

        let frame_target = renderer.create_frame_target(vp[0], vp[1]);

        #[cfg(feature = "metrics")]
        let prim_count = service.topology().len();
        #[cfg(feature = "metrics")]
        let open_metrics = Some(OpenLoadMetrics {
            stage_name: source_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_owned(),
            usd_open_ms,
            ecs_topology_ms,
            usd_viewport_ms,
            ecs_viewport_ms,
            pipeline_compile_ms,
            texture_decode_ms: decode_elapsed.as_secs_f64() * 1000.0,
            texture_upload_ms: upload_elapsed.as_secs_f64() * 1000.0,
            texture_file_read_ms: tex_timings.file_read_ms,
            texture_image_decode_ms: tex_timings.image_decode_ms,
            texture_rgba_convert_ms: tex_timings.rgba_convert_ms,
            texture_cap_resize_ms: tex_timings.cap_resize_ms,
            texture_mip_chain_ms: tex_timings.mip_chain_ms,
            first_render_ms: 0.0,
            total_ms: 0.0,
            prim_count,
            mesh_count,
            texture_count: prefetch_count,
        });

        #[cfg_attr(not(feature = "xr-preview"), allow(unused_mut))]
        let mut session = Self {
            id: SessionId::next(),
            source: SessionSource::Local(source_path.clone()),
            source_path,
            service,
            hydrator,
            renderer,
            frame_target,
            viewport_pixels: vp,
            orbit_camera: OrbitCamera::default_framing(),
            camera_initialized: false,
            render_camera: RenderCameraSource::Orbit,
            selected_path: None,
            editor_overlays_enabled: true,
            diagnostics,
            projection_basis,
            projection_basis_inv,
            xr_recenter: glam::Vec3::ZERO,
            world_unit_scale,
            stage_shadow_mode,
            stage_behavior_run_scope,
            // Default-on capability; the shell clamps it via
            // `set_shadow_capability` from the product profile (off for
            // Inspector/Poser) right after construction.
            shadow_capability: true,
            shadow_tier_cap: idtx_render::ShadowTier::Soft,
            topology_generation: 0,
            visibility_generation: 0,
            change_generation: 0,
            tick_force_extract_count: 0,
            session_visibility_authored: std::collections::HashMap::new(),
            last_snapshot: None,
            snapshot_dirty: true,
            snapshot_transform_patches: None,
            snapshot_skeleton_dirty: false,
            snapshot_widget_dirty: false,
            snapshot_geometry_dirty: false,
            snapshot_geometry_paths: Vec::new(),
            snapshot_topology_splice: None,
            behaviors: BehaviorHost::new(Limits::conductor(), InputRouter::default()),
            authored_behaviors_enabled: false,
            behavior_reconcile_pending: false,
            pending_level_cancellation: false,
            behavior_highlights: Vec::new(),
            behavior_overlay_lines: Vec::new(),
            presentation_overlay_lines: Vec::new(),
            pending_level_transition: None,
            play_state: PlayState::Authoring,
            #[cfg(feature = "authoring")]
            stage_dirty: false,
            #[cfg(feature = "authoring")]
            saved_root_hash: None,
            frame_input: FrameInput::new(),
            mapping_contexts: MappingContextStack::new(),
            #[cfg(feature = "xr-preview")]
            xr_preview: None,
            #[cfg(feature = "xr-bake")]
            xr_controller_mesh: None,
            #[cfg(feature = "xr-preview")]
            xr_hand_mesh: None,
            #[cfg(feature = "xr-preview")]
            xr_hand_viz: xr_hand_mesh::XrHandVisualization::Auto,
            #[cfg(all(feature = "xr-preview", feature = "authoring"))]
            xr_hydrated_controllers: [false, false],
            #[cfg(all(feature = "xr-preview", feature = "authoring"))]
            xr_pose_behavior_owned: [false; 4],
            #[cfg(all(feature = "xr-preview", feature = "authoring"))]
            xr_pose_prim_visible: [true; 4],
            #[cfg(all(feature = "xr-preview", feature = "authoring"))]
            xr_session_dumped: false,
            #[cfg(idtx_xr)]
            xr_gpu: None,
            #[cfg(all(feature = "visionos", target_vendor = "apple"))]
            visionos_gpu: None,
            #[cfg(all(feature = "visionos", target_vendor = "apple"))]
            visionos_anchor: None,
            #[cfg(all(feature = "visionos", target_vendor = "apple"))]
            visionos_anchor_warmup: 0,
            #[cfg(feature = "metrics")]
            t_open_start,
            #[cfg(feature = "metrics")]
            open_metrics,
            #[cfg(feature = "metrics")]
            last_render_frame_ns: 0,
            #[cfg(feature = "metrics")]
            last_extract_ns: 0,
            #[cfg(feature = "metrics")]
            last_gpu_render_ns: 0,
            #[cfg(feature = "metrics")]
            last_anim_sample_ns: 0,
            pseudo_root,
            #[cfg(feature = "physics")]
            physics: None,
            #[cfg(feature = "physics")]
            physics_build_gen: None,
            #[cfg(feature = "physics")]
            pending_physics_control: Vec::new(),
            #[cfg(feature = "physics")]
            physics_dirty_prims: Vec::new(),
            #[cfg(feature = "openexec")]
            exec: None,
            #[cfg(feature = "openexec")]
            exec_build_gen: None,
            #[cfg(feature = "openexec")]
            exec_sources_fp: 0,
            #[cfg(feature = "openexec")]
            exec_did_initial_tick: false,
            #[cfg(feature = "remote")]
            exec_network_policy: ExecNetworkPolicy::denied(),
            auth_scopes: idtx_auth::AuthScopes::default(),
            #[cfg(feature = "remote")]
            collab: None,
            #[cfg(all(feature = "remote", feature = "authoring"))]
            collab_preparation: None,
            #[cfg(feature = "remote")]
            pending_remote_transforms: std::collections::HashMap::new(),
            #[cfg(feature = "remote")]
            collab_notices: Vec::new(),
            #[cfg(feature = "remote")]
            pending_layer_reloads: Vec::new(),
            #[cfg(feature = "remote")]
            collab_recovery: Vec::new(),
            #[cfg(all(feature = "remote", feature = "authoring"))]
            collab_persistence: None,
            #[cfg(all(feature = "remote", feature = "authoring"))]
            collab_applying: false,
            #[cfg(all(feature = "remote", feature = "authoring"))]
            collab_attributes: collab_attributes::AttributeSync::default(),
            #[cfg(all(feature = "metrics", feature = "remote"))]
            collab_tick_metrics: None,
            #[cfg(feature = "metrics")]
            xr_metrics: xr_metrics::XrMetricsAccumulator::new(),
            #[cfg(feature = "metrics")]
            xr_last_author_ns: 0,
            #[cfg(feature = "metrics")]
            xr_last_reproject_ns: 0,
        };
        session.install_scene_backend();
        session.refresh_behavior_data();
        // Build the collision BVH over the freshly-hydrated colliders so the
        // first behaviour raycast of the session already traverses it.
        #[cfg(feature = "collision-bvh")]
        session.refresh_collision_bvh();

        // On-demand open: the assembly was opened with payloads unloaded
        // (`load_payloads == false`); compose+show only the default level now, via
        // the same transition path a runtime level swap uses. Other levels stay
        // uncomposed until a later `transition_level`.
        #[cfg(feature = "authoring")]
        if !load_payloads {
            if let Some(level) = on_demand_default_level {
                session.transition_level(None, level.to_owned(), None)?;
            }
        }
        #[cfg(not(feature = "authoring"))]
        let _ = on_demand_default_level;

        Ok(session)
    }

    /// (Re)builds the collision BVH resource ([`idtx_scene::CollisionBvh`]) from
    /// the current world so [`idtx_scene::picking::pick_prim_filtered`] (reached
    /// via the behaviour [`crate::scene_backend_impl::StudioSceneBackend`]) can
    /// traverse it instead of the O(N) linear scan.
    ///
    /// The BVH is a derived projection of the live `BoundsComp`s — rebuilding
    /// (rather than refitting) after a stage change always reads live bounds, so
    /// the accelerated candidate set can never diverge from the linear scan. One
    /// rebuild per *changed* tick is cheap relative to the several raycasts XR
    /// issues per frame against an otherwise-static scene. Metrics counters are
    /// preserved across rebuilds.
    #[cfg(feature = "collision-bvh")]
    fn refresh_collision_bvh(&mut self) {
        let world = self.hydrator.world_mut();
        if let Some(mut bvh) = world.remove_resource::<idtx_scene::CollisionBvh>() {
            bvh.rebuild(world);
            world.insert_resource(bvh);
        } else {
            let bvh = idtx_scene::CollisionBvh::build(world);
            world.insert_resource(bvh);
        }
    }

    // ── Accessors ─────────────────────────────────────────────────────────────

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    /// Origin of this session's stage (local file vs. remote URL).
    pub fn source(&self) -> &SessionSource {
        &self.source
    }

    /// `true` when the stage was opened from a remote server. Convenience
    /// wrapper over [`SessionSource::is_remote`].
    pub fn is_remote(&self) -> bool {
        self.source.is_remote()
    }

    /// Attach a [`CollabClient`] and begin transform and attribute sync.
    #[cfg(feature = "remote")]
    pub fn attach_collab(&mut self, client: idtx_collab::client::CollabClient) {
        #[cfg(feature = "authoring")]
        if let Err(error) = self.initialize_attribute_sync() {
            client.fail_inbound();
            self.push_collab_notice(format!("Cannot initialize attribute authority: {error}"));
        }
        #[cfg(feature = "authoring")]
        { self.collab_persistence = Some(CollabPersistence {
            observed_root: self.service.root_layer_content_hash().ok(),
            non_transform: self.service.root_non_sync_text().ok(),
            transform_opinions: self.root_transform_opinions().unwrap_or_default(),
            local_revision: 0,
            local_only_dirty: self.stage_dirty || (self.service.root_layer_is_dirty().unwrap_or(true)
                && self.saved_root_hash.is_none_or(|hash| self.service.root_layer_content_hash().ok() != Some(hash))),
            committing: None,
        }); }
        self.collab = Some(client);
        self.pending_remote_transforms.clear();
    }

    /// Detach and return the active [`CollabClient`], if any.
    ///
    /// The caller may close it or retain its operation evidence for recovery.
    #[cfg(feature = "remote")]
    pub fn detach_collab(&mut self) -> Option<idtx_collab::client::CollabClient> {
        self.pending_remote_transforms.clear();
        self.collab.take()
    }

    /// Returns `true` when a [`CollabClient`] is currently attached.
    #[cfg(feature = "remote")]
    pub fn has_collab(&self) -> bool {
        self.collab.is_some()
    }

    /// Request that the collaboration server author a built-in prim of
    /// `type_name` under `parent_path`. Returns `false` (and requests nothing)
    /// when the type is not a server-creatable built-in (Scope / Camera / lights),
    /// so the caller can tell the user it is unavailable in a collab session.
    ///
    /// On success the prim is NOT authored locally here: the server broadcasts a
    /// `CreatePrimBroadcast` (to this client too), which `apply_inbound_collab`
    /// authors into the root layer so the normal hydration flow renders it.
    #[cfg(feature = "remote")]
    pub fn request_remote_prim(&self, parent_path: &str, type_name: &str) -> bool {
        self.collab
            .as_ref()
            .is_some_and(|c| c.send_create_prim(parent_path, type_name))
    }

    /// Request that the collaboration server author a typeless prim under
    /// `parent_path` carrying a **payload** to `reference_path` (a server path
    /// relative to the current stage). Returns `false` when no collab client is
    /// attached or the request could not be enqueued.
    ///
    /// On success nothing is authored locally here: the server broadcasts a
    /// `CreatePrimBroadcast` with `type = Ref` (to this client too), which
    /// `apply_inbound_collab` authors into the root layer as a typeless prim plus
    /// a payload arc — the composed subtree then hydrates and renders on the next
    /// tick. Mirrors [`request_remote_prim`].
    #[cfg(feature = "remote")]
    pub fn request_remote_asset_prim(&self, parent_path: &str, reference_path: &str) -> bool {
        self.collab
            .as_ref()
            .is_some_and(|c| c.send_create_reference(parent_path, reference_path, true))
    }

    /// Request that the collaboration server soft-delete the prim at `path`
    /// (author `active = false`, pruning the prim and its subtree). Returns
    /// `false` when no collab client is attached or the request could not be
    /// enqueued, so the caller can tell the user the deletion was not requested.
    ///
    /// On success nothing is authored locally here: the server broadcasts a
    /// `SetPrimActiveBroadcast` (to this client too), which `apply_inbound_collab`
    /// authors into the root layer — the prim's subtree is then pruned from the
    /// scene tree and render on the next tick. Mirrors [`request_remote_prim`].
    #[cfg(feature = "remote")]
    pub fn request_remote_prim_deletion(&self, path: &str) -> bool {
        self.collab
            .as_ref()
            .is_some_and(|c| c.send_set_prim_active(path, false))
    }

    /// Drain user-facing collaboration notices accumulated since the last call
    /// (e.g. a server-rejected prim create). The shell shows these in its status
    /// line.
    #[cfg(feature = "remote")]
    pub fn take_collab_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.collab_notices)
    }

    /// Drain the server `LayerReloaded` notices accumulated since the last call.
    /// The shell handles each by re-fetching the named file (force-refresh),
    /// reloading it into the stage, and re-gating the collab client on a fresh
    /// snapshot. See [`RemoteEditKind::LayerReloaded`][idtx_collab::client::RemoteEditKind::LayerReloaded].
    #[cfg(feature = "remote")]
    pub fn take_pending_layer_reloads(&mut self) -> Vec<PendingLayerReload> {
        std::mem::take(&mut self.pending_layer_reloads)
    }

    #[cfg(all(feature = "remote", feature = "authoring"))]
    fn push_collab_notice(&mut self, message: String) {
        if self.collab_notices.len() >= 256 { self.collab_notices.remove(0); }
        self.collab_notices.push(message);
    }

    /// Preserved local root/subtree opinions superseded by authoritative replay.
    /// Empty path denotes the whole root layer captured before reconnect.
    #[cfg(feature = "remote")]
    pub fn collab_recovery_opinions(&self) -> &[(String, String)] {
        &self.collab_recovery
    }

    #[cfg(all(feature = "remote", feature = "authoring"))]
    pub fn collab_recovery_report(&self) -> Result<String, SessionError> {
        let client = self.collab.as_ref().ok_or_else(|| SessionError::Collaboration("no collaboration client".into()))?;
        let mut report = client.recovery_report();
        report.push_str("\nCurrent root opinions:\n");
        report.push_str(&self.service.root_layer_text()?);
        for (path, text) in &self.collab_recovery {
            report.push_str(&format!("\nPreserved opinions at {path}:\n{text}\n"));
        }
        Ok(report)
    }

    #[cfg(all(feature = "remote", feature = "authoring"))]
    fn can_retain_collab_opinion(&self, text: &str) -> bool {
        self.collab_recovery.len() < 256
            && self.collab_recovery.iter().map(|(path, text)| path.len() + text.len()).sum::<usize>()
                .saturating_add(text.len()) <= 16 * 1024 * 1024
    }

    /// Retry the retained failed prefix after repairing its prerequisite.
    #[cfg(feature = "remote")]
    pub fn retry_collab_application(&self) {
        if let Some(client) = &self.collab { client.retry_application(); }
    }

    /// Returns `true` when a [`CollabClient`] is attached AND its background
    /// I/O thread is still running.
    ///
    /// Returns `false` after a server-side disconnect or idle timeout kills the
    /// thread.  Use [`reconnect_collab`] to re-establish the connection.
    #[cfg(feature = "remote")]
    pub fn collab_is_connected(&self) -> bool {
        self.collab.as_ref().map_or(false, |c| c.is_connected())
    }

    /// Returns `true` when a [`CollabClient`] is attached and the server has
    /// delivered `SnapshotComplete` — i.e. the client holds the full current
    /// state. Before this, the viewport shows a "syncing" state.
    #[cfg(feature = "remote")]
    pub fn collab_is_synced(&self) -> bool {
        self.collab.as_ref().map_or(false, |c| c.is_synced())
    }

    /// Live status of the attached remote sync session, or `None` when no
    /// collab client is attached (i.e. the stage has no active sync).
    ///
    /// Drives the viewport's remote-sync border: `Syncing` → AMBER,
    /// `ConnectedToPeer` → BLUE, `Hosting` → GREEN, `Disconnected` → RED.
    #[cfg(feature = "remote")]
    pub fn collab_sync_status(&self) -> Option<CollabSyncStatus> {
        self.collab
            .as_ref()
            .map(|c| classify_sync_status(c.is_connected(), c.is_synced(), c.owns_session()))
    }

    /// Re-establish a dropped WebSocket connection to the collaboration server.
    ///
    /// No-op when no collab client is attached.  Returns an error if the new
    /// request cannot be admitted. Network failures are delivered asynchronously.
    #[cfg(feature = "remote")]
    pub fn reconnect_collab(&mut self) -> Result<(), idtx_collab::error::CollabError> {
        #[cfg(feature = "authoring")]
        if self.collab.is_some() {
            let text = self.service.root_layer_text()
                .map_err(|error| idtx_collab::error::CollabError::WebSocket(error.to_string()))?;
            if !self.can_retain_collab_opinion(&text) {
                return Err(idtx_collab::error::CollabError::WebSocket("recovery capacity reached; export opinions before reconnecting".into()));
            }
            self.collab_recovery.push((String::new(), text));
            self.collab_preparation = None;
            self.discard_attribute_predictions()
                .map_err(|error| idtx_collab::error::CollabError::WebSocket(error.to_string()))?;
            self.initialize_attribute_sync()
                .map_err(|error| idtx_collab::error::CollabError::WebSocket(error.to_string()))?;
        }
        if let Some(c) = self.collab.as_mut() {
            c.reconnect()
        } else {
            Ok(())
        }
    }

    /// Commit the active session's overrides to the server's origin file.
    ///
    /// No-op when no collab client is attached.  Returns an error if the
    /// HTTP request fails or the server rejects the commit.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    pub fn commit_collab(&mut self) -> Result<(), SessionError> {
        self.tick()?;
        let client = self.collab.as_ref().ok_or_else(|| SessionError::Collaboration("no collaboration client".into()))?;
        let state = self.collab_persistence.as_mut().ok_or_else(|| SessionError::Collaboration("no persistence state".into()))?;
        if state.committing.is_some() { return Err(SessionError::Collaboration("commit already in progress".into())); }
        let receiver = client.begin_commit().map_err(|e| SessionError::Collaboration(e.to_string()))?;
        state.committing = Some(CollabCommitPending { receiver, local_revision: state.local_revision, intent_revision: client.intent_revision() });
        Ok(())
    }

    #[cfg(all(feature = "remote", feature = "authoring"))]
    pub fn collab_commit_pending(&self) -> bool {
        self.collab_persistence.as_ref().is_some_and(|state| state.committing.is_some())
    }

    /// Nonblocking completion; `true` means the tab may safely close. A successful
    /// commit with local-only/newer edits returns `false` and keeps them dirty.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    pub fn poll_collab_commit(&mut self) -> Option<Result<bool, String>> {
        if !self.collab_commit_pending() { return None; }
        if let Err(error) = self.tick() {
            if let Some(state) = &mut self.collab_persistence { state.committing = None; }
            return Some(Err(error.to_string()));
        }
        let state = self.collab_persistence.as_mut()?;
        let pending = state.committing.as_ref()?;
        let result = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("commit worker disconnected".into()),
        };
        let clean = result.as_ref().is_ok_and(|seq| !state.local_only_dirty && pending.local_revision == state.local_revision
            && self.collab.as_ref().is_some_and(|client| client.is_synced() && client.applied_seq() == *seq && client.intent_revision() == pending.intent_revision));
        state.committing = None;
        if clean { self.mark_stage_saved(); } else { self.stage_dirty = true; }
        Some(result.map(|_| clean))
    }

    #[cfg(all(feature = "remote", feature = "authoring"))]
    fn observe_collab_root(&mut self, remote: bool) -> Result<(), SessionError> {
        #[cfg(feature = "metrics")]
        let started = std::time::Instant::now();
        if self.collab_persistence.is_none() { return Ok(()) }
        let hash = self.service.root_layer_content_hash()?;
        if self.collab_persistence.as_ref().is_some_and(|state| state.observed_root == Some(hash)) { return Ok(()) }
        let other = self.service.root_non_sync_text()?;
        let transforms = self.root_transform_opinions()?;
        let state = self.collab_persistence.as_mut().expect("persistence checked");
        if !remote {
            state.local_revision = state.local_revision.wrapping_add(1);
            state.local_only_dirty |= state.non_transform.as_ref() != Some(&other)
                || state.transform_opinions != transforms;
        }
        state.observed_root = Some(hash);
        state.non_transform = Some(other);
        state.transform_opinions = transforms;
        #[cfg(feature = "metrics")]
        tracing::debug!(remote, elapsed_us = started.elapsed().as_micros() as u64, "collaboration root-opinion classification");
        Ok(())
    }

    #[cfg(all(feature = "remote", feature = "authoring"))]
    fn publish_local_transform(&mut self, path: &str) -> Result<(), SessionError> {
        if self.collab_applying || self.collab.is_none() { return Ok(()) }
        let trs = self.authored_trs_for_outbound(path);
        let matrix = self.get_local_transform_f64(path).ok_or_else(|| StageError::UnknownPath(path.into()))?;
        let client = self.collab.as_ref().expect("collab checked");
        let queued = if let Some((translation, rotation, scale)) = trs {
            client.send_transform_separate(path.into(), translation, rotation, scale)
        } else { client.send_transform(path.into(), matrix) };
        // Suppress the notice echo of an edit already queued at authoring time.
        self.pending_remote_transforms.insert(path.into(), matrix);
        let transforms = self.root_transform_opinions()?;
        if let Some(state) = &mut self.collab_persistence {
            if queued {
                state.transform_opinions = transforms;
            }
            else { state.local_only_dirty = true; }
        }
        Ok(())
    }

    /// Overrides the recorded [`SessionSource`].
    ///
    /// Used after a remote stage finishes loading, to record the originating
    /// URL (the async loader constructs the session with a `Local` source).
    pub fn set_source(&mut self, source: SessionSource) {
        self.source = source;
    }

    /// Records that the stage now lives at `path` on the local filesystem.
    ///
    /// Called after a successful "Save As…" so that subsequent in-place saves,
    /// reloads, and the "Save" affordance target the new local file — including
    /// when the stage was originally opened from a remote server.
    pub fn mark_saved_local(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        self.source = SessionSource::Local(path.clone());
        self.source_path = path;
    }

    /// Short display name — the filename without directory path.
    pub fn label(&self) -> &str {
        self.source_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<unknown>")
    }

    /// Read access to the underlying USD stage service.
    pub fn stage(&self) -> &StageService {
        &self.service
    }

    /// Counter that increments each time a topology change is applied via
    /// [`tick`][Self::tick].  A cached tree is stale when this value differs
    /// from the value seen when the cache was built.
    pub fn topology_generation(&self) -> u64 {
        self.topology_generation
    }

    /// Counter that increments each time a visibility-only change is processed
    /// via [`tick`][Self::tick].  A cached tree is stale when this value or
    /// [`topology_generation`][Self::topology_generation] differs from the
    /// value seen when the cache was built.
    pub fn visibility_generation(&self) -> u64 {
        self.visibility_generation
    }

    /// Counter that increments on every `tick()` call that processed at least
    /// one USD change, regardless of domain.  Use this to invalidate caches that
    /// hold per-prim attribute values (e.g. the inspector property cache).
    pub fn change_generation(&self) -> u64 {
        self.change_generation
    }

    /// Returns the computed visibility for the prim at `path`, as currently
    /// reflected in the ECS world.  Returns `true` (visible) when the prim is
    /// not yet hydrated.
    pub fn prim_visible(&self, path: &str) -> bool {
        let Some(entity) = self.hydrator.entity_for_path(path) else {
            return true;
        };
        self.hydrator
            .world()
            .get::<VisibilityComp>(entity)
            .is_none_or(|v| v.visible)
    }

    /// Returns the effective visibility for the prim at `path` as managed by
    /// the ECS override chain.  `true` when the prim and all its ancestors
    /// are visible (or not yet hydrated).
    ///
    /// This is the same value as [`prim_visible`][Self::prim_visible] and is
    /// kept for call-site compatibility.
    pub fn prim_authored_visible(&self, path: &str) -> bool {
        self.prim_visible(path)
    }

    /// Toggles the visibility of the prim at `path` using the ECS-managed
    /// 3-rule model.  No USD authoring is performed.
    ///
    /// Rule 1: Setting a prim invisible hides all `Inherited` descendants.
    /// Rule 2: A prim invisible due to its parent can be made explicitly
    ///         visible, overriding the parent.
    /// Rule 3: When a prim is made visible, descendant explicit-`Visible`
    ///         overrides are cleared so they follow the parent again.
    pub fn toggle_visibility(&mut self, path: &str) {
        if let Some(entity) = self.hydrator.entity_for_path(path) {
            self.hydrator.toggle_prim_visibility(entity);
            self.visibility_generation = self.visibility_generation.wrapping_add(1);
            self.snapshot_dirty = true;
        }
    }

    /// Forces the ECS visibility of the prim at `path` to an **absolute** state
    /// (`true` → explicitly visible, `false` → invisible), deterministically —
    /// unlike [`toggle_visibility`][Self::toggle_visibility], which flips
    /// relative to the current (possibly mis-seeded) `VisibilityComp`. No USD
    /// authoring is performed. No-op if the prim is not hydrated.
    #[cfg(all(feature = "xr-preview", feature = "authoring"))]
    fn force_prim_visibility(&mut self, path: &str, visible: bool) {
        if let Some(entity) = self.hydrator.entity_for_path(path) {
            self.hydrator.set_prim_visibility_override(entity, visible);
            self.visibility_generation = self.visibility_generation.wrapping_add(1);
            self.snapshot_dirty = true;
        }
    }

    /// Executes a level transition synchronously against the live stage.
    ///
    /// Drives the pure [`LevelTransition`](idtx_editor_model::LevelTransition)
    /// state machine to completion and performs each emitted
    /// [`TransitionStep`](idtx_editor_model::TransitionStep) against the session
    /// layer / payload model:
    /// - `LoadPayload` / `UnloadPayload` → [`StageService::set_payload_loaded`]
    ///   (composes / reclaims the level holder's payload subtree).
    /// - `SetVisible` → [`StageService::author_session_visibility`] (an
    ///   `inherited`/`invisible` opinion in the session layer — never dirties the
    ///   saved assembly).
    ///
    /// Missing prims are skipped (a holder path that does not compose is a no-op,
    /// not an error), so a partially-authored assembly transitions gracefully.
    ///
    /// After authoring every step it re-projects **once** via [`tick`](Self::tick)
    /// so ECS/render reflect the payload + visibility writes. Because the payload
    /// load/unload is a topology change, that tick runs the topology branch, whose
    /// `initialize_visibility_overrides` re-seeds each entity's
    /// `VisibilityOverride` from USD and can reset a just-authored session
    /// visibility back to `Inherited`. The final loop therefore re-asserts each
    /// `SetVisible` directly against the ECS override chain **after** the tick
    /// (inlining [`force_prim_visibility`](Self::force_prim_visibility)'s body,
    /// which is otherwise `xr-preview`-gated).
    ///
    /// This is the synchronous executor: async payload streaming, warm-up masking
    /// beyond the cosmetic transfer level, and a content-hash asset cache across
    /// swaps remain deferred (they need the async `StageLoader` and the
    /// `idtx-assets` cache).
    #[cfg(feature = "authoring")]
    pub fn apply_level_transition(
        &mut self,
        transition: &mut idtx_editor_model::LevelTransition,
    ) -> Result<(), SessionError> {
        use idtx_editor_model::TransitionStep;

        let steps = transition.run_to_completion();

        // 1. Author every step against the live stage (resolve paths fresh; a
        //    miss is a no-op).
        for step in &steps {
            match step {
                TransitionStep::LoadPayload(path) => {
                    if let Some(prim) = self.service.prim_at_path(path) {
                        self.service.set_payload_loaded(prim, true)?;
                    }
                }
                TransitionStep::UnloadPayload(path) => {
                    self.clear_runtime_spawns_under(path)?;
                    if let Some(prim) = self.service.prim_at_path(path) {
                        self.service.set_payload_loaded(prim, false)?;
                    }
                }
                TransitionStep::SetVisible { path, visible } => {
                    if self.service.prim_at_path(path).is_some() {
                        self.service.author_session_visibility(path, *visible)?;
                    }
                }
            }
        }

        // 2. Re-project once (payload changes topology → the superset branch
        //    re-hydrates and re-seeds visibility overrides).
        self.tick()?;

        // 3. Re-assert each SetVisible against the ECS override chain, AFTER the
        //    tick's `initialize_visibility_overrides` has re-seeded Inherited.
        let mut reasserted = false;
        for step in &steps {
            if let TransitionStep::SetVisible { path, visible } = step {
                if let Some(entity) = self.hydrator.entity_for_path(path) {
                    self.hydrator.set_prim_visibility_override(entity, *visible);
                    reasserted = true;
                }
            }
        }
        if reasserted {
            self.visibility_generation = self.visibility_generation.wrapping_add(1);
            self.snapshot_dirty = true;
        }

        Ok(())
    }

    /// Convenience wrapper over [`apply_level_transition`](Self::apply_level_transition):
    /// begins a transition from `from` (`None` on first load) to `to`, optionally
    /// masked by a `transfer` level, and runs it to completion.
    #[cfg(feature = "authoring")]
    pub fn transition_level(
        &mut self,
        from: Option<String>,
        to: String,
        transfer: Option<String>,
    ) -> Result<(), SessionError> {
        let mut transition = idtx_editor_model::LevelTransition::begin(from, to, transfer);
        self.apply_level_transition(&mut transition)
    }

    /// Returns `(render_one_frame_ns, extract_ns, gpu_render_ns)` for the most recent
    /// call to [`render_one_frame`][Self::render_one_frame], along with a reference to
    /// the renderer's own per-phase timing breakdown.
    ///
    /// Only available when the `metrics` feature is enabled.
    #[cfg(feature = "metrics")]
    pub fn last_frame_timings(&self) -> (u64, u64, u64, &idtx_render::metrics::RenderFrameTimings) {
        (
            self.last_render_frame_ns,
            self.last_extract_ns,
            self.last_gpu_render_ns,
            self.renderer.last_frame_timings(),
        )
    }

    /// Nanoseconds spent on animation sampling (USD C++ call + ECS write-back)
    /// during the most recent `tick_animation` call.  Zero when animation is
    /// paused or the stage has no animation.
    #[cfg(feature = "metrics")]
    pub fn last_anim_sample_ns(&self) -> u64 {
        self.last_anim_sample_ns
    }

    /// Per-tick collaboration counters from the most recent `tick()` call.
    ///
    /// `None` when there is no active collab session or the `metrics` / `remote`
    /// features are disabled.
    #[cfg(all(feature = "metrics", feature = "remote"))]
    pub fn collab_tick_metrics(&self) -> Option<&CollabTickMetrics> {
        self.collab_tick_metrics.as_ref()
    }

    /// Returns extraction sub-phase statistics from the most recently extracted snapshot.
    ///
    /// `None` when no snapshot has been extracted yet or the `metrics` feature is disabled.
    #[cfg(feature = "metrics")]
    pub fn last_extract_stats(&self) -> Option<&idtx_render::scene_extract::ExtractionStats> {
        self.last_snapshot.as_ref().map(|s| &s.stats)
    }

    /// Stage-level metadata: up-axis, meters-per-unit, root identifier, etc.
    pub fn metadata(&self) -> &StageMetadata {
        self.service.metadata()
    }

    /// Set the product-level shadow capability. The shell calls this right after
    /// opening a session, passing `ProductProfile::shadows_enabled` (false for
    /// Inspector/Poser). Gates the per-frame [`Self::resolved_shadow_settings`].
    pub fn set_shadow_capability(&mut self, enabled: bool) {
        self.shadow_capability = enabled;
    }

    /// Set the network capability gate for stage-authored REST datasources. The
    /// runtime calls this right after opening a session, passing the policy derived
    /// from its `[network]` config (CLI / `run.toml`). Deny-by-default: without this
    /// call no REST datasource ever polls. Takes effect on the next exec rebuild
    /// (topology change) — call before the first [`Self::tick_exec`].
    ///
    /// The **same** policy also gates the behavior `net` capability: it is pushed
    /// onto the behavior host so a `net`-granted behavior's HTTP is validated by
    /// the identical SSRF allowlist as a REST datasource.
    #[cfg(feature = "remote")]
    pub fn set_exec_network_policy(&mut self, policy: ExecNetworkPolicy) {
        self.behaviors.set_network_policy(policy.clone());
        self.exec_network_policy = policy;
    }

    /// Set the independent credential-sharing policy before attaching behaviors.
    pub fn set_auth_origins(&mut self, origins: &[String]) -> Result<(), idtx_auth::AuthError> {
        let scopes = idtx_auth::AuthScopes::parse(origins)?;
        self.behaviors.set_auth_scopes(scopes.clone());
        self.auth_scopes = scopes;
        Ok(())
    }

    /// Enable or disable editor overlays (world-axis lines) at runtime. Overlays
    /// default to on. The out-of-process `idtx-runtime` preview player disables
    /// them so preview frames never draw editor chrome, even when the
    /// `editor-overlay` feature is compiled into the shared `idtx-render` rlib via
    /// cargo feature unification.
    pub fn set_editor_overlays_enabled(&mut self, enabled: bool) {
        self.editor_overlays_enabled = enabled;
        self.renderer.set_editor_overlays_enabled(enabled);
    }

    /// Clamp the maximum shadow tier for the target hardware. Desktop leaves this
    /// at [`ShadowTier::Soft`]; XR callers (Quest 2/3/Pro) lower it to `Smooth` or
    /// `Off` where the expensive tiers cannot render within budget.
    pub fn set_shadow_tier_cap(&mut self, cap: idtx_render::ShadowTier) {
        self.shadow_tier_cap = cap;
    }

    /// Resolve the shadow configuration for this frame from the three inputs: the
    /// stage `idtx:shadowMode` (`Off`/`Hard`/`Smooth`/`Soft`), the product
    /// capability, and the target-hardware tier cap. The authored mode picks a
    /// desired tier (`Hard`→`Hard`, `Smooth`→`Smooth`, `Soft`→`Soft`); the
    /// hardware cap clamps it downward; the product capability forces `Off`
    /// entirely. See [`resolve_shadow_settings`].
    #[must_use]
    pub fn resolved_shadow_settings(&self) -> idtx_render::RenderShadowSettings {
        resolve_shadow_settings(
            self.stage_shadow_mode,
            self.shadow_capability,
            self.shadow_tier_cap,
        )
    }

    /// The stage's live shadow mode — the session-owned copy, updated by
    /// [`Self::set_stage_shadow_mode`] (the cached [`StageMetadata`] is *not*
    /// refreshed by an author write, so this is the source of truth for the UI).
    #[must_use]
    pub fn stage_shadow_mode(&self) -> idtx_usd::ShadowMode {
        self.stage_shadow_mode
    }

    /// Author a new stage shadow mode into the root layer's `customLayerData`
    /// (`idtx:shadowMode` token, legacy bool erased), then live-update the
    /// session-owned copy and mark the snapshot dirty so the next frame
    /// re-resolves [`Self::resolved_shadow_settings`] and re-extracts. This is
    /// the Stage-tab authoring entry point (Conductor only).
    #[cfg(feature = "authoring")]
    pub fn set_stage_shadow_mode(
        &mut self,
        mode: idtx_usd::ShadowMode,
    ) -> Result<(), SessionError> {
        self.service.author_root_shadow_mode(mode)?;
        self.stage_shadow_mode = mode;
        self.snapshot_dirty = true;
        Ok(())
    }

    /// The render world unit scale currently in effect.
    /// One renderer unit = `world_unit_scale().metres_per_unit()` real metres.
    pub fn world_unit_scale(&self) -> WorldUnitScale {
        self.world_unit_scale
    }

    /// Changes the render world unit scale and recomputes the coordinate-system
    /// correction matrix from the stage's current metadata.
    ///
    /// Takes effect on the next call to [`render_one_frame`].
    pub fn set_world_unit_scale(&mut self, scale: WorldUnitScale) {
        self.world_unit_scale = scale;
        let meta = self.service.metadata();
        let up_axis = UpAxis::from_usd_token(&meta.up_axis);
        self.projection_basis = pseudo_root_matrix(scale, meta.meters_per_unit, up_axis);
        self.projection_basis_inv = self.projection_basis.inverse();
        self.install_scene_backend();
        self.snapshot_dirty = true;
    }

    /// Diagnostic messages from the most-recent viewport projection.
    pub fn asset_diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    /// Current offscreen render target dimensions in pixels `[width, height]`.
    pub fn viewport_pixels(&self) -> [u32; 2] {
        self.viewport_pixels
    }

    /// The rendered color texture view.  Register this with
    /// `egui_wgpu::Renderer::register_native_texture` (or update it with
    /// `update_egui_texture_from_wgpu_texture`) to display the viewport inside
    /// an egui image widget.
    pub fn display_view(&self) -> &wgpu::TextureView {
        &self.frame_target.color_view
    }

    /// Read the current display frame back to CPU as tightly-packed
    /// `Rgba8UnormSrgb` pixels (row stride = `width * 4`).
    ///
    /// Exists only for the headless perf harness so `session` mode can dump PNGs
    /// exactly like `render` mode; it is not part of the interactive display path
    /// (the shell samples `display_view()` on the GPU). Behind `metrics` so the
    /// default shell/app build never compiles the readback plumbing.
    #[cfg(feature = "metrics")]
    pub fn read_pixels(&self) -> Result<Vec<u8>, idtx_render::RenderError> {
        self.renderer.read_pixels(&self.frame_target)
    }

    /// Read access to the ECS world (e.g. for render extraction).
    pub fn world(&self) -> &idtx_scene::bevy_ecs::world::World {
        self.hydrator.world()
    }

    /// Mutable access to the ECS world.
    ///
    /// Used by an out-of-process runtime player to attach and tick a general
    /// behavior host against the same world the session renders from. The
    /// session itself never mutates the world outside its own tick/hydration
    /// path, so this is a deliberate seam for the runtime layer.
    pub fn world_mut(&mut self) -> &mut idtx_scene::bevy_ecs::world::World {
        self.hydrator.world_mut()
    }

    /// The network role that governs behavior execution for the prim at `path`,
    /// resolved by walking to its nearest [`PlayerRole`](idtx_scene::PlayerRole)
    /// ancestor.
    ///
    /// Mirrors exactly what the authority gate
    /// ([`attach_authored_behaviors`]) sees, so the inspector can warn that a
    /// behavior on a [`Remote`](idtx_scene::NetRole::Remote) (or unopted
    /// [`Server`](idtx_scene::NetRole::Server)) pawn will not tick locally.
    /// Returns [`NetRole::Local`](idtx_scene::NetRole::Local) when no
    /// `PlayerRole` ancestor exists (the single-player default) and `None` only
    /// when `path` has no hydrated entity.
    #[must_use]
    pub fn resolved_player_role(&self, path: &str) -> Option<idtx_scene::NetRole> {
        let entity = self.hydrator.entity_for_path(path)?;
        Some(idtx_scene::resolve_owning_player_role(
            self.hydrator.world(),
            entity,
        ))
    }

    // ── Rendering ─────────────────────────────────────────────────────────────

    /// Inject the transient XR controller meshes into the cached snapshot when
    /// an XR preview is active.
    ///
    /// The controller geometry is stable across frames (a real mesh sourced from
    /// the invisible `/XR/Controllers/{Left,Right}` proxy, or the fallback
    /// capsule), so the renderer reuses its GPU buffer after the first upload.
    /// Called before every scene render that feeds a headset or the desktop
    /// mirror so the controllers track live.
    ///
    /// Legacy mesh-bake path — gated behind `xr-bake` (off by default). The
    /// default path hydrates controllers as visible prefabs through the ordinary
    /// hydrator path (see `xr_controller_prefab`), so this injection is not compiled in a
    /// default build.
    #[cfg(feature = "xr-bake")]
    fn inject_xr_controller_meshes(&mut self) {
        let Some(xr) = self.xr_preview.as_ref() else {
            return;
        };
        let cache = self
            .xr_controller_mesh
            .get_or_insert_with(xr_controller_mesh::ControllerMeshCache::new);
        let world = self.hydrator.world();
        let left_mat = xr.controller_world_matrix(world, true);
        let right_mat = xr.controller_world_matrix(world, false);
        // Determine whether each hand is currently tracked so the vis-mode can
        // decide whether to show controllers or suppress them.
        let (left_hand_active, right_hand_active) = {
            let (l, r) = xr.platform().hand_poses();
            (l.is_some(), r.is_some())
        };
        let show_left = self.xr_hand_viz.show_controller(left_hand_active);
        let show_right = self.xr_hand_viz.show_controller(right_hand_active);
        // Suppress the legacy bake for any hand rendered through the hydrated
        // path so a controller is never drawn twice.
        #[cfg(feature = "authoring")]
        let (show_left, show_right) = (
            show_left && !self.xr_hydrated_controllers[0],
            show_right && !self.xr_hydrated_controllers[1],
        );
        let Some(snap) = self.last_snapshot.as_mut() else {
            return;
        };
        // Materials: patch each part's material from the snapshot's
        // pre-resolved USD material map (populated without visibility filtering,
        // so controller material prims ARE present). Guarded by snapshot.serial
        // so this is a no-op on frames where the snapshot hasn't changed.
        // Retries automatically if the snapshot was stale (render_xr called
        // before render_one_frame re-extracted the XR assembly snapshot).
        cache.fill_materials(&snap.materials, snap.serial);
        // Strip any controller instances from a prior frame (the snapshot is
        // reused when USD has not changed) and push the current pose. Without the
        // strip the instances would accumulate by two on every non-dirty frame.
        xr_controller_mesh::apply_controller_instances(
            cache,
            left_mat,
            right_mat,
            show_left,
            show_right,
            &mut snap.meshes,
        );
    }

    /// Inject hand-skeleton bone/joint geometry for both hands.
    ///
    /// Called after `inject_xr_controller_meshes` so the hand geometry appears
    /// in the same snapshot pass. Both hands are collapsed into **two**
    /// hardware-instanced batches (one bone prototype, one joint prototype) so
    /// the whole ~100-bone/joint skeleton costs two draws + two GPU cache
    /// entries instead of ~100 (B4). The renderer's instanced-batch cache is
    /// serial-guarded ([renderer.rs](../../idtx-render/src/renderer.rs)), so the
    /// snapshot serial is bumped whenever hand batches are added or removed to
    /// force the per-instance transform buffers to re-upload / evict.
    #[cfg(feature = "xr-preview")]
    fn inject_xr_hand_meshes(&mut self) {
        let Some(xr) = self.xr_preview.as_ref() else {
            return;
        };
        let (left_pose, right_pose) = xr.platform().hand_poses();
        // Clone poses so we can borrow `self` mutably for the cache.
        let left_pose = left_pose.cloned();
        let right_pose = right_pose.cloned();

        let left_active = left_pose.is_some();
        let right_active = right_pose.is_some();

        // Placement that maps the raw play-space joint positions into the virtual
        // world exactly like the eye cameras: `projection_basis *
        // worldMatrix(/World/Player)` (locomotion + snap-turn yaw) with the
        // play-space recenter offset removed. Hand joints are located by
        // `xrLocateHandJointsEXT` in play (STAGE) space and are not authored USD
        // prims under the pawn — unlike the hydrated controller child-prims, which
        // inherit the pawn transform through USD composition — so without this they
        // render frozen at their raw tracked position while the player moves. Must
        // be computed before the `last_snapshot` borrow below (needs `hydrator`).
        // Identity when no local-player pawn resolves, matching `render_xr`'s eye
        // path (and `xr_recenter` is `ZERO` in that case).
        let placement = {
            let basis = self.projection_basis;
            let recenter = self.xr_recenter;
            let stage_to_world = local_player_root_world(self.hydrator.world_mut())
                .map_or(glam::Mat4::IDENTITY, |m| basis * m);
            stage_to_world * glam::Mat4::from_translation(-recenter)
        };

        let Some(snap) = self.last_snapshot.as_mut() else {
            return;
        };
        // Remove last frame's hand batches from the reused snapshot; remember
        // whether anything was actually present so we can force an evict on the
        // active→inactive transition even when no new batches are added.
        let had_batches = xr_hand_mesh::strip_hand_instanced_batches(&mut snap.instanced_batches);

        if !left_active && !right_active {
            // Hands just disappeared: bump the instance serial once so the renderer
            // evicts the orphaned bone/joint GPU entries.
            if had_batches {
                snap.instance_serial = snap.instance_serial.wrapping_add(1);
            }
            return;
        }

        let cache = self
            .xr_hand_mesh
            .get_or_insert_with(xr_hand_mesh::HandMeshCache::new);

        // Accumulate both hands' per-instance transforms into the two shared
        // batches, honouring the visualisation mode's per-hand gating.
        let mut joint_transforms: Vec<glam::Mat4> = Vec::new();
        let mut bone_transforms: Vec<glam::Mat4> = Vec::new();
        if self.xr_hand_viz.show_hand(left_active) {
            if let Some(pose) = &left_pose {
                xr_hand_mesh::push_hand_transforms(
                    pose,
                    placement,
                    &mut joint_transforms,
                    &mut bone_transforms,
                );
            }
        }
        if self.xr_hand_viz.show_hand(right_active) {
            if let Some(pose) = &right_pose {
                xr_hand_mesh::push_hand_transforms(
                    pose,
                    placement,
                    &mut joint_transforms,
                    &mut bone_transforms,
                );
            }
        }

        let batches = cache.build_batches(joint_transforms, bone_transforms);
        if batches.is_empty() {
            // Vis-mode suppressed both hands this frame; evict if needed.
            if had_batches {
                snap.instance_serial = snap.instance_serial.wrapping_add(1);
            }
            return;
        }
        snap.instanced_batches.extend(batches);
        // Hand poses change every frame; force the serial-guarded instance cache
        // to re-upload the transform buffers (and warm the cache on first sight).
        // Bump only `instance_serial`, NOT `serial`: the latter also gates the
        // whole-scene static draw-merge rebuild, so bumping it every tracked frame
        // re-baked and re-uploaded all mergeable static geometry per frame — the
        // drastic XR hand-tracking framerate drop. The merge cache's own
        // `xform_sig` warm re-bake still moves any folded debug gizmos.
        snap.instance_serial = snap.instance_serial.wrapping_add(1);
    }

    /// Get the current hand visualisation mode.
    #[cfg(feature = "xr-preview")]
    pub fn hand_visualization(&self) -> xr_hand_mesh::XrHandVisualization {
        self.xr_hand_viz
    }

    /// Set the hand visualisation mode (controller vs hand skeleton vs auto).
    #[cfg(feature = "xr-preview")]
    pub fn set_hand_visualization(&mut self, mode: xr_hand_mesh::XrHandVisualization) {
        self.xr_hand_viz = mode;
    }

    /// Enables or disables the **hydrated-controller** path for one hand
    /// (see `docs/xr-controller-hydration-plan.md`).
    ///
    /// Rather than baking + re-injecting a merged mesh, the existing (invisible)
    /// `/XR/Controllers/{hand}` proxy — already composed with the controller
    /// reference + upAxis/MPU normalization by `preview_assembly` — is flipped
    /// visible via a **session-layer `over`** and driven per frame by
    /// [`render_xr`]/[`render_one_frame`] through the ordinary
    /// hydrate → extract path (skeleton, GeomSubset materials, authored
    /// transform intact). The legacy bake is suppressed for the hand.
    ///
    /// Idempotent. `left = true` selects the left hand. The USD session layer
    /// holds the authoritative visibility opinion; the ECS render state is then
    /// forced to match **deterministically** via
    /// [`force_prim_visibility`][Self::force_prim_visibility].
    ///
    /// The ECS write is authoritative for *rendering* and is intentionally an
    /// absolute set, not a toggle: authored USD invisibility is **not** reliably
    /// reflected in the ECS after hydration (a pre-existing seeding-order defect —
    /// `apply_viewport_projection` pre-seeds `VisibilityOverride::Inherited` for
    /// every imageable prim and `initialize_visibility_overrides` then skips it,
    /// so the authored-invisible proxy resolves to `Inherited`→visible and in
    /// fact *renders* at its authored pose today). A relative toggle against that
    /// mis-seeded value could flip the wrong way; forcing the absolute state is
    /// correct regardless. See `docs/xr-controller-hydration-plan.md` §4.
    #[cfg(all(feature = "xr-preview", feature = "authoring"))]
    pub fn set_controller_hydrated(
        &mut self,
        left: bool,
        hydrated: bool,
    ) -> Result<(), SessionError> {
        let idx = usize::from(!left);
        if self.xr_hydrated_controllers[idx] == hydrated {
            return Ok(());
        }
        let path = if left {
            xr_controller_prefab::LEFT_CONTROLLER_PATH
        } else {
            xr_controller_prefab::RIGHT_CONTROLLER_PATH
        };
        // Authoritative visibility opinion in the USD session layer (keeps the
        // single-source-of-truth invariant), then force the ECS render state to
        // match — an absolute set, since reprojection never re-derives the
        // sticky override and the authored value is not honored at seed time.
        self.service.author_session_visibility(path, hydrated)?;
        self.force_prim_visibility(path, hydrated);
        self.xr_hydrated_controllers[idx] = hydrated;
        Ok(())
    }

    /// Whether the hydrated-controller path is active for a hand.
    #[cfg(all(feature = "xr-preview", feature = "authoring"))]
    #[must_use]
    pub fn controller_hydrated(&self, left: bool) -> bool {
        self.xr_hydrated_controllers[usize::from(!left)]
    }

    /// Whether an authored WASM behavior owns the prim at `path` — i.e. a live
    /// behavior instance is attached to that prim's ECS entity. Used to enforce
    /// single-authority for the XR pose prims: a behavior-owned controller /
    /// pointer is driven solely by its authored `*_controller_sync` /
    /// `*_aim_sync` behavior, so the session's hydrated pose write stands down.
    #[cfg(all(feature = "xr-preview", feature = "authoring"))]
    #[must_use]
    fn xr_pose_prim_behavior_owned(&self, path: &str) -> bool {
        self.hydrator.entity_for_path(path).is_some_and(|entity| {
            self.behaviors
                .has_instance_for_entity(idtx_behavior::entity_to_wire(entity))
        })
    }

    /// Authors the current tracked pose of each hydrated controller onto its
    /// pose Xform (the `/XR/Controllers/{hand}` proxy root) as a session-layer
    /// transform, then reprojects so the skinned/rigid controller follows.
    ///
    /// `controller_world_matrix` is render/stage space; snapshot extraction
    /// re-applies `projection_basis` (USD→render), so the authored USD-space
    /// pose is `projection_basis_inv * render_matrix` — the pre-multiply cancels
    /// `projection_basis` for the whole subtree, letting the proxy's
    /// render-aligned normalization place the geometry directly.
    ///
    /// No-op when no hand is hydrated or no hand is tracked this frame (an
    /// untracked hand keeps its last authored pose). Routed through
    /// [`tick`][Self::tick] → `project_viewport_roots` — the exact value-only
    /// seam the plan specifies — whose skeleton-dependency expansion pulls the
    /// controller skeleton in and recomputes its world pose.
    #[cfg(all(feature = "xr-preview", feature = "authoring"))]
    fn author_hydrated_controller_poses(&mut self) {
        let hydrated = self.xr_hydrated_controllers;
        // Single-authority: when an authored `*_controller_sync` / `*_aim_sync`
        // behavior owns a hand's prim, it is the sole pose driver — the session
        // must not also write that prim (indices: 0/1 = L/R controller, 2/3 =
        // L/R pointer). Read before any field borrow to preserve the dance.
        let owned = self.xr_pose_behavior_owned;
        if !hydrated.iter().any(|&h| h) {
            return;
        }
        let basis_inv = self.projection_basis_inv;
        // Gather (path, USD column-major matrix) under the immutable borrows,
        // then author under the mutable service borrow (disjoint-field dance).
        let mut writes: Vec<(&'static str, [f64; 16])> = Vec::new();
        {
            let Some(xr) = self.xr_preview.as_ref() else {
                return;
            };
            let world = self.hydrator.world();
            for (idx, path) in [
                (0usize, xr_controller_prefab::LEFT_CONTROLLER_PATH),
                (1usize, xr_controller_prefab::RIGHT_CONTROLLER_PATH),
            ] {
                if !hydrated[idx] || owned[idx] {
                    continue;
                }
                if let Some(render_mat) = xr.controller_world_matrix(world, idx == 0) {
                    let usd_mat = basis_inv * render_mat;
                    writes.push((path, usd_mat.to_cols_array().map(f64::from)));
                }
            }
            // Pointer rays ride the aim pose (distinct from grip). Authored onto
            // the sibling `/XR/Pointers/*` prims via the same USD-space pre-multiply
            // so the pointer follows the ergonomic aim direction, not the grip.
            for (idx, path) in [
                (0usize, xr_controller_prefab::LEFT_POINTER_PATH),
                (1usize, xr_controller_prefab::RIGHT_POINTER_PATH),
            ] {
                if owned[2 + idx] {
                    continue;
                }
                if let Some(render_mat) = xr.aim_world_matrix(idx == 0) {
                    let usd_mat = basis_inv * render_mat;
                    writes.push((path, usd_mat.to_cols_array().map(f64::from)));
                }
            }
        }
        if writes.is_empty() {
            return;
        }
        // One-shot: dump the session-authored `over`s (visibility + pose
        // matrices) to the console before the first XR frame consumes them.
        self.dump_session_overrides(&writes);
        for (path, matrix) in writes {
            if let Err(e) = self.service.author_session_transform(path, matrix) {
                tracing::warn!("author hydrated controller pose {path}: {e}");
            }
        }
        // The value-only reprojection that picks these poses up (skeleton follows
        // the new ancestor pose; `snapshot_dirty` set for the next extract) is run
        // **once** by the caller `render_xr` immediately after this returns, so it
        // also drains the dynamic physics poses `tick_physics` authored earlier
        // this frame (which defers its own drain on the XR path) — one
        // `drain_changes` + reprojection per XR frame instead of two.
    }

    /// Hides a hand's controller + pointer prims while that hand is actively
    /// hand-tracked, and restores them when tracking drops — so a hand-tracked
    /// hand renders only its skeleton, not a controller alongside it.
    ///
    /// Per-hand and gated on *live pose presence* (`hand_poses().is_some()`, the
    /// same `hand_active` signal `inject_xr_hand_meshes` uses), via the current
    /// [`XrHandVisualization`]: `Auto` hides a controller when its hand is tracked;
    /// `ControllerOnly` always shows controllers; `HandSkeletonOnly` always hides
    /// them. The hand-skeleton injection reads the same mode, so controllers and
    /// skeletons never both show for the same hand.
    ///
    /// Edge-triggered: a visibility `over` is authored only when the resolved
    /// state differs from [`xr_pose_prim_visible`], so steady state costs nothing.
    /// Uses the same authoritative mechanism as [`set_controller_hydrated`]
    /// (session-layer `author_session_visibility` + absolute
    /// [`force_prim_visibility`]); the latter sets `snapshot_dirty`, so the caller's
    /// next extract reflects the change without a dedicated reprojection.
    ///
    /// Independent of behavior pose-ownership: the `*_controller_sync` behaviors
    /// own the controller *pose*, this owns its *visibility*.
    #[cfg(all(feature = "xr-preview", feature = "authoring"))]
    fn apply_xr_hand_controller_visibility(&mut self) {
        let (left_active, right_active) = {
            let Some(xr) = self.xr_preview.as_ref() else {
                return;
            };
            let (l, r) = xr.platform().hand_poses();
            (l.is_some(), r.is_some())
        };
        let viz = self.xr_hand_viz;
        // (state index, prim path, this hand's live-pose flag).
        let targets = [
            (
                0usize,
                xr_controller_prefab::LEFT_CONTROLLER_PATH,
                left_active,
            ),
            (
                1usize,
                xr_controller_prefab::RIGHT_CONTROLLER_PATH,
                right_active,
            ),
            (2usize, xr_controller_prefab::LEFT_POINTER_PATH, left_active),
            (
                3usize,
                xr_controller_prefab::RIGHT_POINTER_PATH,
                right_active,
            ),
        ];
        for (idx, path, hand_active) in targets {
            let want_visible = viz.show_controller(hand_active);
            if self.xr_pose_prim_visible[idx] == want_visible {
                continue;
            }
            // Authoritative USD opinion first (session layer), then force the ECS
            // render state. A missing prim (controllers not composed in this scene)
            // makes `author_session_visibility` fail and `force_prim_visibility`
            // a no-op; latch the state regardless so we do not retry every frame.
            if let Err(e) = self.service.author_session_visibility(path, want_visible) {
                tracing::debug!("xr controller visibility {path}: {e}");
            }
            self.force_prim_visibility(path, want_visible);
            self.xr_pose_prim_visible[idx] = want_visible;
        }
    }

    /// Prints the session-authored controller `over`s once, the first time a
    /// hydrated-controller pose is authored (before the first XR frame consumes
    /// it). Dumps the Rust-tracked session state — each hand's authored
    /// visibility and pose matrix (USD column-major, as authored onto the
    /// session-layer transform) — so the composed session `over`s can be
    /// eyeballed against the plan without adding a C++ FFI to read back the
    /// composed stage. Latched by `xr_session_dumped`, so it fires at most once
    /// per session. See `docs/xr-controller-hydration-plan.md` §5.
    #[cfg(all(feature = "xr-preview", feature = "authoring"))]
    fn dump_session_overrides(&mut self, writes: &[(&'static str, [f64; 16])]) {
        if self.xr_session_dumped {
            return;
        }
        self.xr_session_dumped = true;
        eprintln!("[xr-session] session-authored controller overrides (first frame):");
        for (hand_idx, hand_name, path) in [
            (0usize, "Left", xr_controller_prefab::LEFT_CONTROLLER_PATH),
            (1usize, "Right", xr_controller_prefab::RIGHT_CONTROLLER_PATH),
        ] {
            eprintln!(
                "[xr-session]   {hand_name} ({path}): visibility={}",
                if self.xr_hydrated_controllers[hand_idx] {
                    "visible (over)"
                } else {
                    "inherited (no over)"
                },
            );
        }
        for (path, matrix) in writes {
            // Row-per-column for a readable 4×4 (USD authors column-major).
            eprintln!("[xr-session]   pose over {path}:");
            for col in 0..4 {
                eprintln!(
                    "[xr-session]     ( {:.4}, {:.4}, {:.4}, {:.4} )",
                    matrix[col * 4],
                    matrix[col * 4 + 1],
                    matrix[col * 4 + 2],
                    matrix[col * 4 + 3],
                );
            }
        }
    }

    /// Render subsequent frames through the authored camera at `path` instead
    /// of the synthetic orbit camera.
    ///
    /// The camera descriptor is recomputed from the camera prim's live ECS
    /// [`WorldTransform`] every frame, so a behavior driving the camera (e.g.
    /// the desktop pitch/yaw mouse-look, or an XR HMD-sync) is reflected. If
    /// the path resolves to no camera on a given frame (e.g. removed by a
    /// resync), [`render_one_frame`][Self::render_one_frame] falls back to the
    /// orbit camera. The editor opts in on stage open via
    /// [`initialize_viewport_camera`][Self::initialize_viewport_camera] (to
    /// render through an authored stage camera 1:1); the runtime opts in via
    /// [`Self::local_player_camera_path`].
    pub fn use_scene_camera(&mut self, path: impl Into<String>) {
        self.render_camera = RenderCameraSource::ScenePath(path.into());
    }

    /// Revert to the synthetic orbit camera for the desktop frame path.
    pub fn use_orbit_camera(&mut self) {
        self.render_camera = RenderCameraSource::Orbit;
    }

    /// The USD path of the stage's authored camera to open through, if any.
    ///
    /// Queries the hydrated ECS for camera prims ([`CameraComp`]) and returns
    /// the lexicographically-first path, for a deterministic pick (ECS archetype
    /// iteration order is not stable). idtx-usd already emits a diagnostic when
    /// a stage authors more than one camera. Unlike
    /// [`local_player_camera_path`](Self::local_player_camera_path) this is NOT
    /// gated on a player role — it is the editor's "use the stage camera" source
    /// consumed by [`initialize_viewport_camera`](Self::initialize_viewport_camera).
    fn first_scene_camera_path(&mut self) -> Option<String> {
        use idtx_scene::CameraComp;

        let world = self.hydrator.world_mut();
        let mut query = world.query::<(&PrimNode, &CameraComp)>();
        let mut paths: Vec<String> = query.iter(world).map(|(node, _)| node.path.clone()).collect();
        paths.sort();
        paths.into_iter().next()
    }

    /// The USD path of the **local player's** camera, if the stage has one.
    ///
    /// A camera qualifies when it has an explicitly authored [`PlayerRole`]
    /// ancestor whose role is [`NetRole::Local`]
    /// (via [`nearest_authored_player_role`][idtx_scene::nearest_authored_player_role]) —
    /// a plain scene camera with no player role does **not** qualify, so the
    /// orbit fallback is preserved. Among qualifying cameras the pawn camera
    /// ([`PLAYER_CAMERA_PATH`][xr_controller_prefab::PLAYER_CAMERA_PATH]) is
    /// preferred, else the first in query order. Shared with the XR-preview
    /// camera resolution (see [`local_player_camera`]), so the desktop and XR
    /// paths render through the same camera.
    pub fn local_player_camera_path(&mut self) -> Option<String> {
        local_player_camera(self.hydrator.world_mut()).map(|(_, path)| path)
    }

    /// The camera descriptor injected into the last rendered frame, if any.
    ///
    /// Reflects the resolution priority in [`render_one_frame`][Self::render_one_frame]
    /// (XR preview → opted-in scene camera → orbit fallback). Exposed so tests
    /// can assert which camera the runtime rendered through and that its `view`
    /// tracks a driving behavior.
    #[must_use]
    pub fn last_render_camera(&self) -> Option<&RenderCameraDesc> {
        self.last_snapshot.as_ref().and_then(|s| s.camera.as_ref())
    }

    /// The local-player pawn root's render-space world matrix,
    /// `projection_basis * worldMatrix(/World/Player)`, or `None` when no
    /// local-player pawn root resolves (a stage with no player rig).
    ///
    /// This is the single `stage_to_world` the XR eye cameras compose, so the
    /// rendered viewpoint follows the locomotion + snap-turn yaw `xr_player_move`
    /// authors on `/World/Player` — matching how the hydrated pawn children
    /// (camera, controllers, capsule) already follow it through USD composition.
    /// Read from the live ECS [`WorldTransform`] each frame. The ECS resolution is
    /// factored into [`local_player_root_world`] so it unit-tests without a GPU.
    #[cfg(idtx_xr)]
    fn local_player_root_matrix(&mut self) -> Option<Mat4> {
        let basis = self.projection_basis;
        local_player_root_world(self.hydrator.world_mut()).map(|m| basis * m)
    }

    /// The current play-space recenter offset (stage-space ground vector). `ZERO`
    /// outside XR / before the first snap. See [`Self::set_xr_recenter`].
    #[must_use]
    pub fn xr_recenter(&self) -> glam::Vec3 {
        self.xr_recenter
    }

    /// Set the play-space recenter offset. The `OpenXR` tick computes this itself
    /// (in [`Self::tick_behaviors`]); the visionOS runtime — which drives its own
    /// behavior host and does not call `tick_behaviors` — computes it with
    /// [`recenter_step`] and sets it here each frame so `visionos_render_frame`
    /// subtracts the same offset from the per-eye pose.
    pub fn set_xr_recenter(&mut self, recenter: glam::Vec3) {
        self.xr_recenter = recenter;
    }

    /// Read a USD-referenced asset authored on `prim_path`'s asset-valued
    /// attribute `attr` (e.g. `idtx:behavior:module` / `idtx:behavior:manifest`) through
    /// `OpenUSD`'s asset resolver.
    ///
    /// Package-aware (reads `stage.usdz[inner.wasm]` without unpacking),
    /// layer-relative, and URI-scheme agnostic (http(s), …) — the same
    /// resolver-first read [`Self::attach_authored_behaviors`] performs
    /// internally. Exposed so an embedder that drives its *own* behavior host
    /// (the visionOS immersive runtime) resolves behavior modules the same way
    /// instead of direct filesystem access, which cannot reach assets packed
    /// inside a `.usdz`. Returns `None` when the prim, the attribute, or the
    /// asset cannot be resolved or read.
    #[must_use]
    pub fn read_prim_asset_via_resolver(&self, prim_path: &str, attr: &str) -> Option<Vec<u8>> {
        read_asset_via_resolver(&self.service, prim_path, attr)
    }

    /// Build the render camera descriptor for the active
    /// [`RenderCameraSource::ScenePath`], or `None` for
    /// [`RenderCameraSource::Orbit`] or when the path no longer resolves to a
    /// camera (caller then falls back to the orbit camera).
    ///
    /// Reads the camera prim's live [`CameraComp`] + [`WorldTransform`] from the
    /// ECS world so a behavior driving the camera is reflected each frame.
    fn scene_camera_desc(&mut self, aspect: f32) -> Option<RenderCameraDesc> {
        use idtx_scene::CameraComp;

        let RenderCameraSource::ScenePath(path) = &self.render_camera else {
            return None;
        };
        let path = path.clone();
        let basis = self.projection_basis;
        let world = self.hydrator.world_mut();
        let mut query = world.query::<(&PrimNode, &CameraComp, &WorldTransform)>();
        let (node, cam, xform) = query.iter(world).find(|(node, _, _)| node.path == path)?;
        Some(RenderCameraDesc::from_scene_camera(
            node.path.clone(),
            cam,
            xform.matrix,
            basis,
            aspect,
        ))
    }

    /// The camera the desktop viewport is **actually** rendered through this
    /// frame: the opted-in authored scene camera ([`scene_camera_desc`]) when
    /// `render_camera` is a [`RenderCameraSource::ScenePath`], else the synthetic
    /// orbit camera.
    ///
    /// This is the single source of truth for the viewport camera. Both the
    /// render frame ([`render_one_frame`][Self::render_one_frame]) and the pick
    /// ray ([`camera_view_proj`][Self::camera_view_proj]) resolve the camera
    /// through here, so a viewport click always unprojects through exactly the
    /// matrix the image was drawn with — including after the user orbits/pans/
    /// dollies or snaps to an orthographic front/left/top preset, each of which
    /// hands off to the orbit camera via
    /// [`adopt_orbit_from_active_camera`](Self::adopt_orbit_from_active_camera).
    fn active_camera_desc(&mut self, aspect: f32) -> RenderCameraDesc {
        self.scene_camera_desc(aspect)
            .unwrap_or_else(|| self.orbit_camera.to_camera_desc(aspect))
    }

    /// Render-space anchor for the visionOS immersive viewer: the local pawn
    /// ROOT's placement, `(projection_basis * worldMatrix(root)).inverse()`, where
    /// `root` is the nearest local-player ancestor of the bound scene camera.
    ///
    /// Composed into each eye's view matrix in
    /// [`visionos_render_frame`](Self::visionos_render_frame): the ARKit head pose
    /// (the per-eye pose) is treated as a pose in the pawn root's local frame, so
    /// the viewer sits at the root's authored world transform and head motion adds
    /// the eye offset on top. Read from the root prim's live [`WorldTransform`]
    /// every frame, so `xr_player_move` moving the pawn root (locomotion +
    /// snap-turn + the play-space recenter bake) moves the viewer.
    ///
    /// **Anchors to the ROOT, not the camera prim** — parity with the `OpenXR` eye
    /// path ([`local_player_root_matrix`](Self::local_player_root_matrix)). The
    /// `/World/Player/Camera` child is driven by `hmd_camera_sync` with the raw HMD
    /// pose, which the per-eye pose already carries; anchoring to the root keeps
    /// that write out of the viewpoint so head motion is not double-counted, while
    /// the root's locomotion/recenter still moves the viewer. The eye subtracts the
    /// play-space recenter (see [`visionos_render`]'s `eye_camera`), so a recenter
    /// snap relocates the capsule without moving the view.
    ///
    /// Returns `None` unless [`render_camera`](Self::render_camera) is
    /// [`RenderCameraSource::ScenePath`] (a local-player camera was bound via
    /// [`use_scene_camera`](Self::use_scene_camera)) and the pawn root resolves —
    /// the caller then falls back to the AABB-centring anchor
    /// ([`compute_visionos_anchor`][self::visionos_render]).
    #[cfg(all(feature = "visionos", target_vendor = "apple"))]
    #[must_use]
    pub fn visionos_scene_camera_anchor(&mut self) -> Option<Mat4> {
        use idtx_scene::bevy_ecs::prelude::Entity;
        use idtx_scene::{NetRole, nearest_authored_player_root};

        // A local-player rig is active iff a scene camera was bound. Resolve the
        // camera entity that activated this path, then anchor to the nearest
        // local-player ROOT above it — NOT a hard-coded `/World/Player`. This way
        // a local camera authored under a non-canonical pawn anchors to its own
        // rig, and an unrelated `/World/Player` prim is never mistaken for the
        // player root. Parity with the OpenXR eye path ([`local_player_root_world`])
        // which likewise requires the root's nearest authored role to be `Local`.
        let RenderCameraSource::ScenePath(path) = &self.render_camera else {
            return None;
        };
        let path = path.clone();
        let basis = self.projection_basis;
        let world = self.hydrator.world_mut();

        // Resolve the active camera's entity by path (collect out of the query
        // iterator so its borrow ends before the ancestor walk re-borrows world).
        let mut query = world.query::<(Entity, &PrimNode)>();
        let cam_entity = query
            .iter(world)
            .find(|(_, node)| node.path == path)
            .map(|(e, _)| e)?;

        // Walk to the nearest ancestor (inclusive) carrying an authored player
        // role; anchor only when that root is the local player.
        let (root_entity, role) = nearest_authored_player_root(world, cam_entity)?;
        if role != NetRole::Local {
            return None;
        }
        let matrix = world.get::<WorldTransform>(root_entity)?.matrix;
        Some(visionos_render::scene_camera_anchor(basis, matrix))
    }

    /// Extracts a render snapshot from the ECS world, auto-frames the camera on
    /// first call, and submits a GPU render to the offscreen texture.
    ///
    /// Extraction is skipped on frames where the ECS world has not changed since
    /// the last call — the cached snapshot is reused instead.  Only the injected
    /// orbit-camera descriptor is refreshed every frame.
    pub fn render_one_frame(&mut self, wireframe: bool) {
        #[cfg(feature = "metrics")]
        let t_frame = std::time::Instant::now();

        let aspect = self.viewport_pixels[0] as f32 / self.viewport_pixels[1] as f32;

        // Author the tracked pose of any hydrated controller onto its
        // USD pose Xform and reproject (drives the desktop-mirror preview too).
        #[cfg(all(feature = "xr-preview", feature = "authoring"))]
        {
            self.author_hydrated_controller_poses();
            // Keep controller/pointer visibility in sync with hand tracking on the
            // desktop-mirror path too (no-op unless a hand is supplying a pose).
            self.apply_xr_hand_controller_visibility();
        }

        // Re-extract only when the scene has changed or no snapshot exists yet.
        #[cfg(feature = "metrics")]
        let t_extract = std::time::Instant::now();
        self.prepare_presentation(aspect);
        #[cfg(feature = "metrics")]
        let extract_ns = t_extract.elapsed().as_nanos() as u64;

        // Choose the viewport camera on the first frame with geometry: the
        // stage's authored camera if present, else a 45°/45° fit of the scene
        // (see `initialize_viewport_camera`). The shell also calls this on open;
        // whichever runs first wins (it is idempotent and re-tries while the
        // stage has neither a camera nor hydrated geometry yet).
        if !self.camera_initialized {
            self.initialize_viewport_camera();
        }

        // Inject the active camera into the cached snapshot every frame.
        // Priority:
        //   1. When the runtime opted into a scene camera (`use_scene_camera`,
        //      typically the local player's), render through it — recomputed
        //      from the camera prim's live `WorldTransform` each frame so its
        //      driving behavior (mouse-look / HMD-sync) is reflected. During a
        //      (non-pcvr) OpenXR preview the HMD-sync behavior authored on
        //      `/World/Player/Camera` drives that prim's `WorldTransform`, so
        //      the desktop mirror follows the headset through this same path —
        //      there is no separate XR-preview camera. (On the real pcvr path
        //      this function does not run while the preview is active — the
        //      mirror is a blit of the left eye, see `render_xr`.)
        //   2. Otherwise the synthetic orbit camera (editor always; runtime
        //      fallback). It has a valid `default_framing()` state even before
        //      geometry arrives, so empty stages still render a navigable view.
        let cam = self.active_camera_desc(aspect);
        self.last_snapshot.as_mut().unwrap().camera = Some(cam);

        // Inject transient controller mesh instances when the legacy
        // bake path is compiled in (`xr-bake`, off by default). The default path renders
        // controllers as hydrated prefabs instead, so a default build injects
        // nothing here. The capsule geometry is stable across frames so the
        // renderer reuses the GPU buffer after the first upload.
        #[cfg(feature = "xr-bake")]
        self.inject_xr_controller_meshes();
        #[cfg(feature = "xr-preview")]
        self.inject_xr_hand_meshes();

        let selection_obb = self
            .selected_path
            .as_deref()
            .and_then(|p| selection_obb_for_path(&self.hydrator, p, self.projection_basis));

        // Editor-only skeleton bone glyphs; empty (and compiled out) in the runtime.
        #[cfg(feature = "editor-overlay")]
        let mut extra = self.skeleton_bone_lines();
        #[cfg(not(feature = "editor-overlay"))]
        let mut extra: Vec<idtx_render::OverlayLine> = Vec::new();
        // Editor-only light-blue collision-shape wireframe for the selected prim
        // (inspector/poser/conductor). Compiled out of the runtime player.
        #[cfg(feature = "editor-overlay")]
        extra.extend(self.collision_wireframe_lines());
        // Light-type selection glyph (ungated: editor and runtime). Appended after
        // the bone lines so both share the single overlay pass.
        extra.extend(self.selected_light_glyph_lines());
        // Behavior-drawn world-space overlay lines (teleport pointer arc + landing
        // marker, from the `overlay` draw channel). Ungated — the runtime player
        // runs behaviors too.
        extra.extend_from_slice(&self.presentation_overlay_lines);

        #[cfg(feature = "metrics")]
        let t_gpu = std::time::Instant::now();
        self.renderer.render_with_overlay(
            &self.frame_target,
            self.last_snapshot.as_ref().unwrap(),
            selection_obb,
            &extra,
            wireframe,
        );
        #[cfg(feature = "metrics")]
        {
            self.last_gpu_render_ns = t_gpu.elapsed().as_nanos() as u64;
            self.last_extract_ns = extract_ns;
            self.last_render_frame_ns = t_frame.elapsed().as_nanos() as u64;
        }
    }

    /// Resizes the offscreen render target.  The existing `egui::TextureId`
    /// remains valid; call `update_egui_texture_from_wgpu_texture` afterwards
    /// to point it at the new texture view.
    pub fn resize_viewport(&mut self, pixels: [u32; 2]) {
        let pixels = [pixels[0].max(1), pixels[1].max(1)];
        if pixels == self.viewport_pixels {
            return;
        }
        self.viewport_pixels = pixels;
        self.frame_target = self.renderer.create_frame_target(pixels[0], pixels[1]);
    }

    // ── Camera navigation ─────────────────────────────────────────────────────

    /// Chooses the viewport camera when a stage is opened.
    ///
    /// Renders through the stage's authored camera if one exists (matching the
    /// authored framing 1:1, so the viewport agrees with an offline Storm
    /// render of the same stage); otherwise frames all geometry from a 45°/45°
    /// top-right synthetic view.
    ///
    /// Idempotent (guards on `camera_initialized`) and only acts while the
    /// camera source is still the default [`RenderCameraSource::Orbit`], so it
    /// never overrides a runtime that has already opted into a scene (player)
    /// camera. When the stage has neither a camera nor hydrated geometry yet
    /// (progressive payload load) it leaves `camera_initialized` false so a
    /// later frame retries.
    pub fn initialize_viewport_camera(&mut self) {
        if self.camera_initialized {
            return;
        }
        if self.render_camera != RenderCameraSource::Orbit {
            // Runtime already opted into a scene camera; nothing to choose.
            self.camera_initialized = true;
            return;
        }
        if let Some(path) = self.first_scene_camera_path() {
            self.use_scene_camera(path);
            self.camera_initialized = true;
        } else if let Some((usd_min, usd_max)) = compute_scene_aabb_from_ecs(&self.hydrator) {
            let (min, max) = transform_aabb(usd_min, usd_max, self.projection_basis);
            self.orbit_camera.frame_aabb(min.into(), max.into());
            self.camera_initialized = true;
        }
        // else: no camera and no geometry yet — retry on a later frame.
    }

    /// Hands off from an authored scene camera to free orbit, seeding the orbit
    /// pose from the camera's current view so the viewport doesn't jump. Called
    /// when the user starts navigating (orbit/pan/dolly/view-change) while
    /// rendering through a scene camera. No-op when already on the orbit camera.
    fn adopt_orbit_from_active_camera(&mut self) {
        if self.render_camera == RenderCameraSource::Orbit {
            return;
        }
        let aspect = self.viewport_pixels[0] as f32 / self.viewport_pixels[1].max(1) as f32;
        if let Some(desc) = self.scene_camera_desc(aspect) {
            let world = desc.view.inverse();
            let eye = world.col(3).truncate();
            let forward = -world.col(2).truncate();
            let target = match compute_scene_aabb_from_ecs(&self.hydrator) {
                Some((usd_min, usd_max)) => {
                    let (min, max) = transform_aabb(usd_min, usd_max, self.projection_basis);
                    (Vec3::from(min) + Vec3::from(max)) * 0.5
                }
                None => eye + forward * self.orbit_camera.radius,
            };
            self.orbit_camera.set_from_eye_target(eye, target);
        }
        self.use_orbit_camera();
    }

    /// Orbits the camera by `delta_yaw` (horizontal, radians) and
    /// `delta_pitch` (vertical, radians).
    /// Exits orthographic mode so free orbit always gives perspective — matching Blender.
    pub fn orbit_camera(&mut self, delta_yaw: f32, delta_pitch: f32) {
        self.adopt_orbit_from_active_camera();
        self.orbit_camera.orbit(delta_yaw, delta_pitch);
        self.orbit_camera.projection_mode = ProjectionMode::Perspective;
    }

    /// Pans the camera target by screen-space deltas (in world-space units
    /// scaled by the orbit radius).
    pub fn pan_camera(&mut self, delta_right: f32, delta_up: f32) {
        self.adopt_orbit_from_active_camera();
        let scale = self.orbit_camera.radius * 0.001;
        self.orbit_camera.pan(delta_right * scale, delta_up * scale);
    }

    /// Dollies (zooms) the camera by multiplying the orbit radius by `factor`.
    pub fn dolly_camera(&mut self, factor: f32) {
        self.adopt_orbit_from_active_camera();
        self.orbit_camera.dolly(factor);
    }

    /// Repositions the orbit camera to frame all scene geometry, taking the
    /// viewport back to the orbit camera if a scene camera was active.
    pub fn frame_all(&mut self) {
        if let Some((usd_min, usd_max)) = compute_scene_aabb_from_ecs(&self.hydrator) {
            let (min, max) = transform_aabb(usd_min, usd_max, self.projection_basis);
            self.orbit_camera.frame_aabb(min.into(), max.into());
        }
        self.use_orbit_camera();
    }

    /// Snaps the viewport camera to a named canonical view and switches to
    /// orthographic projection, matching Blender's numpad view conventions.
    pub fn set_view_preset(&mut self, preset: ViewPreset) {
        self.adopt_orbit_from_active_camera();
        self.orbit_camera.set_view_preset(preset);
    }

    /// Toggles between perspective and orthographic projection.
    pub fn toggle_projection(&mut self) {
        self.adopt_orbit_from_active_camera();
        self.orbit_camera.toggle_projection();
    }

    /// Returns the current projection mode of the orbit camera.
    pub fn projection_mode(&self) -> ProjectionMode {
        self.orbit_camera.projection_mode
    }

    /// Returns the named view the camera is currently snapped to, or `None`
    /// when the camera is in a free-orbit (perspective) view.
    pub fn current_view_preset(&self) -> Option<ViewPreset> {
        self.orbit_camera.current_view
    }

    // ── Selection ─────────────────────────────────────────────────────────────

    /// Sets the selected prim path.  Pass `None` to deselect.
    pub fn set_selected(&mut self, path: Option<String>) {
        self.selected_path = path;
    }

    /// Currently selected prim path, if any.
    pub fn selected_path(&self) -> Option<&str> {
        self.selected_path.as_deref()
    }

    /// Returns the authored property records for the given prim path.
    ///
    /// Returns an empty `Vec` when the path is not present in the live stage
    /// (unknown path, not-yet-loaded payload, etc.).  The call crosses the FFI
    /// boundary once per invocation; callers should cache the result and refresh
    /// only when the selection or `topology_generation` changes.
    pub fn inspect_prim(
        &mut self,
        path: &str,
    ) -> Result<Vec<idtx_usd::PropertyRecord>, SessionError> {
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(vec![]);
        };
        Ok(self.service.inspect_prim(prim_id)?)
    }

    /// Returns the skeleton joint records for a `Skeleton` or `SkelRoot` prim at `path`.
    ///
    /// Looks up the ECS entity by path and retrieves the [`SkeletonTopologyComp`].
    /// Returns `None` when the prim is not a skeleton or has not been hydrated yet.
    pub fn skeleton_joints_for_path(
        &self,
        path: &str,
    ) -> Option<Vec<idtx_usd::SkeletonJointRecord>> {
        let entity = self.hydrator.entity_for_path(path)?;
        let comp = self.hydrator.world().get::<SkeletonTopologyComp>(entity)?;
        Some(comp.record.joints.clone())
    }

    /// Builds octahedral bone-glyph overlay lines for the currently selected
    /// skeleton, in render world space.
    ///
    /// Editor-only (`editor-overlay` feature) — the packaged runtime never
    /// enables it, so bone glyphs never appear in the player. Returns empty
    /// unless the selected prim carries a hydrated [`SkeletonTopologyComp`] and
    /// [`SkeletonPoseComp`].
    ///
    /// Joint world positions are read from the per-frame [`SkeletonPoseComp`]
    /// (`joint_world_transforms`), which the animation system re-samples every
    /// frame — so the glyphs follow playback automatically.
    ///
    /// `joint_world_transforms` are already full stage-world space: the C++
    /// bridge computes them via `UsdSkelSkeletonQuery::ComputeJointWorldTransforms`
    /// with an `xformCache`, which folds in the skeleton prim's local-to-world
    /// (and therefore every ancestor/`SkelRoot` transform). So only
    /// `projection_basis` is applied here — multiplying by
    /// `skeleton_world_transform` as well would double-apply the ancestor chain.
    #[cfg(feature = "editor-overlay")]
    pub fn skeleton_bone_lines(&self) -> Vec<idtx_render::OverlayLine> {
        use idtx_scene::hydrator::mat4_from_usd_row_major;

        let mut lines = Vec::new();
        let Some(path) = self.selected_path.as_deref() else {
            return lines;
        };
        let Some(entity) = self.hydrator.entity_for_path(path) else {
            return lines;
        };
        let world = self.hydrator.world();
        let (Some(topo), Some(pose)) = (
            world.get::<SkeletonTopologyComp>(entity),
            world.get::<SkeletonPoseComp>(entity),
        ) else {
            return lines;
        };

        let joints = &topo.record.joints;
        let joint_xforms = &pose.record.joint_world_transforms;
        if joints.len() != joint_xforms.len() {
            return lines;
        }

        // `joint_world_transforms` are already stage-world space (ancestor
        // transforms folded in by the bridge); only project into render space.
        let basis = self.projection_basis;
        let positions: Vec<Vec3> = joint_xforms
            .iter()
            .map(|m| (basis * mat4_from_usd_row_major(m)).transform_point3(Vec3::ZERO))
            .collect();

        // Mid-to-light blue, matching the editor's active-selection accent family.
        let color = [0.40, 0.65, 1.0, 1.0];
        for (i, joint) in joints.iter().enumerate() {
            if let Some(parent) = joint.parent {
                idtx_render::push_bone_octahedron(
                    &mut lines,
                    positions[parent as usize].to_array(),
                    positions[i].to_array(),
                    color,
                );
            }
        }
        lines
    }

    /// The prim's **local-space** axis-aligned bounds (`BoundsComp::local_minimum`
    /// / `local_maximum`), or `None` when the path has no hydrated entity or
    /// bounds. The collision inspector uses this to seed mesh-derived default
    /// dimensions when a collision shape is chosen (so an analytic collider is
    /// sized to the prim's geometry rather than the schema's generic fallbacks).
    #[must_use]
    pub fn collision_local_bounds(&self, path: &str) -> Option<([f32; 3], [f32; 3])> {
        let entity = self.hydrator.entity_for_path(path)?;
        let bounds = self.hydrator.world().get::<idtx_scene::BoundsComp>(entity)?;
        Some((bounds.local_minimum, bounds.local_maximum))
    }

    /// Builds the light-blue collision-shape wireframe for the currently selected
    /// prim, when it carries an `IdtxCollisionAPI` ([`idtx_scene::CollisionComp`]).
    ///
    /// Analytic shapes (`sphere`/`cube`/`cylinder`/`capsule`) are sized from the
    /// prim's local bounds; `hull` draws the authored hull triangle soup; `mesh`
    /// draws the prim's own render-mesh outline. The wireframe uses the exact same
    /// geometry the picking narrow-phase tests against, placed by
    /// `projection_basis * worldTransform`.
    ///
    /// Editor-only: compiled out of the runtime player (the `editor-overlay`
    /// feature), and additionally suppressed when editor overlays are disabled at
    /// runtime (preview frames). Returns empty when nothing collidable is selected.
    #[cfg(feature = "editor-overlay")]
    pub fn collision_wireframe_lines(&self) -> Vec<idtx_render::OverlayLine> {
        let mut lines = Vec::new();
        if !self.editor_overlays_enabled {
            return lines;
        }
        let Some(path) = self.selected_path.as_deref() else {
            return lines;
        };
        let Some(entity) = self.hydrator.entity_for_path(path) else {
            return lines;
        };
        let world = self.hydrator.world();
        let Some(col) = world.get::<idtx_scene::CollisionComp>(entity) else {
            return lines;
        };

        let world_matrix = world
            .get::<idtx_scene::WorldTransform>(entity)
            .map_or(self.projection_basis, |w| self.projection_basis * w.matrix);
        let color = idtx_render::COLLISION_COLOR;

        match col.shape {
            idtx_scene::CollisionShape::Hull => {
                idtx_render::push_collision_trisoup(
                    &mut lines,
                    world_matrix,
                    &col.hull_points,
                    &col.hull_triangles,
                    color,
                );
            }
            idtx_scene::CollisionShape::Mesh => {
                if let Some(mesh) = world.get::<idtx_scene::MeshComp>(entity) {
                    idtx_render::push_collision_polysoup(
                        &mut lines,
                        world_matrix,
                        &mesh.points,
                        &mesh.face_vertex_counts,
                        &mesh.face_vertex_indices,
                        color,
                    );
                }
            }
            analytic => {
                // Resolve dimensions (authored, else from the prim's local
                // bounds) exactly as the picking narrow-phase does, so the
                // wireframe matches the collider — Cone flows through here too.
                let geom = world.get::<idtx_scene::BoundsComp>(entity).map(|bounds| {
                    idtx_scene::collision_shape_geom(bounds.local_minimum, bounds.local_maximum)
                });
                if let Some(resolved) =
                    idtx_scene::resolve_analytic(analytic, &col.dims, geom)
                {
                    idtx_render::push_collision_analytic(
                        &mut lines,
                        resolved,
                        world_matrix,
                        color,
                    );
                }
            }
        }
        lines
    }

    /// Builds the DCC-style selection glyph for the currently selected light, if
    /// any. Lights carry no `BoundsComp`, so the corner-tick OBB overlay draws
    /// nothing for them; instead we emit a light-type-specific wireframe (sun
    /// rays, wire sphere, spot cone, rect/disk with normal, cylinder, dome).
    ///
    /// The light's `world_matrix` in [`RenderLightDesc`] is already in render
    /// space (the projection basis was folded in during extraction), so it is
    /// used directly. Returns an empty vector when nothing (or a non-light) is
    /// selected. Ungated: available in both editor and runtime builds.
    pub fn selected_light_glyph_lines(&self) -> Vec<idtx_render::OverlayLine> {
        let mut lines = Vec::new();
        let Some(path) = self.selected_path.as_deref() else {
            return lines;
        };
        let Some(snapshot) = self.last_snapshot.as_ref() else {
            return lines;
        };
        let Some(light) = snapshot.lights.iter().find(|l| l.prim_path == path) else {
            return lines;
        };
        // Scene scale for the otherwise dimensionless distant/dome glyphs and the
        // spot cone depth; the orbit radius tracks the framed scene size.
        let scene_radius = self.orbit_camera.radius;
        // Warm yellow — distinct from the orange OBB corner-ticks used for meshes.
        idtx_render::push_light_glyph(
            &mut lines,
            &idtx_render::LightGlyphParams {
                kind: light.kind,
                world_matrix: light.world_matrix,
                cone_angle: light.cone_angle,
                cone_softness: light.cone_softness,
                scene_radius,
                color: [1.0, 0.9, 0.2, 1.0],
            },
        );
        lines
    }

    ///
    /// The FFI bridge resolves inherited material bindings automatically, so
    /// passing a Mesh or Xform prim returns the bound material's graph. When
    /// no material is found, the returned `MaterialGraph` has `material_path =
    /// None` and an empty `nodes` list.
    ///
    /// Returns an empty graph (not an error) when the path is unknown to the
    /// stage. Callers should cache the result and refresh only when the
    /// selection or `topology_generation` changes.
    pub fn material_graph_for_path(
        &mut self,
        path: &str,
    ) -> Result<idtx_usd::MaterialGraph, SessionError> {
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(idtx_usd::MaterialGraph {
                material_path: None,
                material_paths: vec![],
                nodes: vec![],
                diagnostics: vec![],
                material_output_connections: vec![],
            });
        };
        Ok(self.service.material_graph(prim_id)?)
    }

    /// Returns the `OpenExec` compute graph rooted at the given prim path by
    /// walking authored attribute connections backward (see the C++
    /// `compute_graph` shim). Returns a graph with `root_path == None` (nothing
    /// to show) when the path is unknown or the prim has no connected attribute.
    pub fn compute_graph_for_path(
        &mut self,
        path: &str,
    ) -> Result<idtx_usd::ComputeGraph, SessionError> {
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(idtx_usd::ComputeGraph {
                root_path: None,
                nodes: vec![],
                diagnostics: vec![],
            });
        };
        Ok(self.service.compute_graph(prim_id)?)
    }

    /// Returns the variant sets on the given prim path.
    ///
    /// Returns an empty `Vec` when the path is unknown to the stage.
    pub fn variant_sets(
        &mut self,
        path: &str,
    ) -> Result<Vec<idtx_usd::VariantSetRecord>, SessionError> {
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(vec![]);
        };
        Ok(self.service.variant_sets(prim_id)?)
    }

    /// Returns the stage's composed layer stack (session + root + sublayers),
    /// strongest-first, for the Layer Stack panel.
    ///
    /// Read-only and requested on demand; the shell caches the result and
    /// refreshes it only when `topology_generation` changes, so this never runs
    /// on the per-frame path.
    pub fn layer_stack(&self) -> Result<Vec<idtx_usd::LayerRecord>, SessionError> {
        Ok(self.service.layer_stack()?)
    }

    /// Reloads a single `SdfLayer` from disk by identifier (the Layer Stack
    /// panel's per-layer reload action).
    ///
    /// USD fires `ObjectsChanged` notices for anything the reload changed, which
    /// are drained by the next `tick()` call — updating the ECS, scene tree, and
    /// render world automatically.
    pub fn reload_layer(&mut self, layer_identifier: &str) -> Result<(), SessionError> {
        self.service.reload_layer(layer_identifier)?;
        Ok(())
    }

    /// Authors a variant selection on the session layer for `path`.
    ///
    /// USD fires `ObjectsChanged { Resynced }` for the prim, which is drained
    /// by the next `tick()` call.  The resulting topology resync updates the
    /// ECS, scene tree, and render world automatically.
    ///
    /// Silently no-ops when `path` is not present in the current stage.
    pub fn set_session_variant(
        &mut self,
        path: &str,
        set_name: &str,
        selection: &str,
    ) -> Result<(), SessionError> {
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(());
        };
        self.service
            .set_session_variant(prim_id, set_name, selection)?;
        Ok(())
    }

    #[cfg(feature = "authoring")]
    pub fn session_variant_opinion(&self, path: &str, set: &str) -> Result<Option<String>, SessionError> {
        Ok(self.service.session_variant_opinion(path, set)?)
    }

    #[cfg(feature = "authoring")]
    pub fn replay_session_variant(&mut self, path: &str, set: &str, expected: Option<&str>, desired: Option<&str>) -> Result<(), SessionError> {
        if self.session_variant_opinion(path, set)?.as_deref() != expected {
            return Err(StageError::OpenUsd("undo/redo conflict in session variant selection".into()).into());
        }
        Ok(self.service.restore_session_variant_opinion(path, set, desired)?)
    }

    /// Returns the local-to-parent transform matrix for `path` at the default time code.
    ///
    /// Uses `UsdGeomXformable::GetLocalTransformation()` via FFI. Returns `None` when the
    /// path is not known to the stage.
    pub fn get_local_transform(&mut self, path: &str) -> Option<[[f32; 4]; 4]> {
        let prim_id = self.service.prim_at_path(path)?;
        self.service.get_local_transform(prim_id).ok()
    }

    /// Exact double-precision local transform for authoring/protocol consumers.
    pub fn get_local_transform_f64(&mut self, path: &str) -> Option<[f64; 16]> {
        let prim = self.service.prim_at_path(path)?;
        self.service.get_local_transform_f64(prim).ok()
    }

    /// Exact double-precision world transform for authoring/protocol consumers.
    pub fn get_world_transform_f64(&mut self, path: &str) -> Option<[f64; 16]> {
        let prim = self.service.prim_at_path(path)?;
        self.service.get_world_transform_f64(prim).ok()
    }

    /// Returns the world-space transform (USD world space, row-major) for the prim at `path`.
    /// `None` when the path is not known to the stage.
    pub fn get_world_transform(&mut self, path: &str) -> Option<[[f32; 4]; 4]> {
        let prim_id = self.service.prim_at_path(path)?;
        self.service.get_world_transform(prim_id).ok()
    }

    /// Returns the world-space transform of the direct parent of `path` (or identity if
    /// `path` has no xformable parent).  Useful for computing local deltas in the gizmo.
    pub fn get_parent_world_transform(&mut self, path: &str) -> Option<[[f32; 4]; 4]> {
        let parent = match path.rfind('/') {
            Some(idx) if idx > 0 => &path[..idx],
            _ => {
                return Some([
                    [1., 0., 0., 0.],
                    [0., 1., 0., 0.],
                    [0., 0., 1., 0.],
                    [0., 0., 0., 1.],
                ]);
            }
        };
        let id = self.service.prim_at_path(parent)?;
        self.service.get_world_transform(id).ok().or(Some([
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ]))
    }

    /// Returns the orbit camera's current orbital radius.  Used by the gizmo to compute
    /// a constant screen-size axis length independent of scene scale.
    pub fn camera_radius(&self) -> f32 {
        self.orbit_camera.radius
    }

    /// Returns `true` when the prim at `path` is-a `UsdGeomXformable`.
    ///
    /// Backed by the [`Xformable`] ECS marker component set at topology hydration time
    /// from `prim.IsA<UsdGeomXformable>()` — no FFI on the hot path, works for
    /// built-in and custom schema types alike.  Returns `false` for unknown paths.
    pub fn is_prim_xformable(&self, path: &str) -> bool {
        self.hydrator
            .entity_for_path(path)
            .is_some_and(|e| self.hydrator.world().get::<Xformable>(e).is_some())
    }

    /// Returns the coordinate-system correction matrix (USD world space → render world space).
    pub fn projection_basis(&self) -> Mat4 {
        self.projection_basis
    }

    /// Author a typed attribute value on the root layer for the prim at `path`.
    ///
    /// Dispatches `StructuralCommand::SetProperty` through the stage service.
    /// USD fires `ObjectsChanged` which is drained by the next `tick()` call,
    /// updating the ECS and render world automatically.
    ///
    /// Silently no-ops when `path` is not present in the current stage.
    #[cfg(feature = "authoring")]
    pub fn author_attribute(
        &mut self,
        path: &str,
        property: &str,
        value: idtx_usd::PropertyValue,
    ) -> Result<(), SessionError> {
        self.author_attribute_targeted(path, property, value, &idtx_usd::EditTarget::Root)
    }

    /// Author a typed attribute value into an explicit [`idtx_usd::EditTarget`].
    ///
    /// [`EditTarget::Root`](idtx_usd::EditTarget::Root) is the historical
    /// behavior — a bare prim-local opinion published through the collaboration
    /// attribute channel. [`EditTarget::Variant`](idtx_usd::EditTarget::Variant)
    /// and [`EditTarget::Session`](idtx_usd::EditTarget::Session) are deliberately
    /// local: they bypass the attribute sync channel entirely (it is keyed purely
    /// by scene path and cannot represent a variant/session coordinate, so a
    /// publish would mis-record the edit as a cleared default — see
    /// `collab_attributes`). The edit still re-composes and re-renders via the
    /// normal drain.
    ///
    /// Silently no-ops when `path` is not present in the current stage.
    #[cfg(feature = "authoring")]
    pub fn author_attribute_targeted(
        &mut self,
        path: &str,
        property: &str,
        value: idtx_usd::PropertyValue,
        target: &idtx_usd::EditTarget,
    ) -> Result<(), SessionError> {
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(());
        };
        #[cfg(feature = "remote")]
        let root_target = matches!(target, idtx_usd::EditTarget::Root);
        #[cfg(feature = "remote")]
        if root_target && !self.collab_applying {
            self.observe_collab_root(false)?;
            self.capture_attribute_authority(path, property)?;
        }
        self.service
            .author_targeted_property(prim_id, property, &value, target)?;
        // Variant/session edits never enter the attribute sync channel.
        #[cfg(feature = "remote")]
        if root_target {
            if property == "xformOpOrder" || property.starts_with("xformOp:") {
                self.publish_local_transform(path)?;
            } else {
                self.publish_attributes(path, &[property.to_owned()])?;
            }
        }
        Ok(())
    }

    /// Author a full TRS transform edit on the root layer for the prim at `path`.
    ///
    /// The C++ `apply_transform_edit` inspects the prim's existing xformOps:
    /// - If the stack contains individual translate/rotate/scale ops, each op
    ///   is authored separately so other ops remain untouched.
    /// - If the stack contains a matrix (`xformOp:transform`), a new composed
    ///   matrix is built from the provided TRS and written.
    ///
    /// USD fires `ObjectsChanged` which is drained by the next `tick()` call,
    /// updating the ECS and render world automatically.
    ///
    /// Silently no-ops when `path` is not present in the current stage.
    #[cfg(feature = "authoring")]
    pub fn author_transform(
        &mut self,
        path: &str,
        translation: [f64; 3],
        rotation_xyz_degrees: [f64; 3],
        scale: [f64; 3],
    ) -> Result<(), SessionError> {
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(());
        };
        #[cfg(feature = "remote")]
        if !self.collab_applying { self.observe_collab_root(false)?; }
        self.service.apply_transform_edit(
            prim_id,
            idtx_usd::TransformValues {
                translation,
                rotation_xyz_degrees,
                scale,
            },
        )?;
        #[cfg(feature = "remote")]
        self.publish_local_transform(path)?;
        Ok(())
    }

    /// Author an exact local affine matrix, preserving its double precision.
    #[cfg(feature = "authoring")]
    pub fn author_matrix_transform(&mut self, path: &str, matrix: [f64; 16]) -> Result<(), SessionError> {
        #[cfg(feature = "remote")]
        if !self.collab_applying { self.observe_collab_root(false)?; }
        let prim = self.service.prim_at_path(path).ok_or_else(|| StageError::UnknownPath(path.into()))?;
        self.service.apply_matrix_transform(prim, matrix)?;
        #[cfg(feature = "remote")]
        self.publish_local_transform(path)?;
        Ok(())
    }

    /// Place the player at a level's spawn point, authored into the **session**
    /// layer (never dirties the saved stage). The level-transition driver
    /// calls this on a freshly loaded level before swapping it in.
    ///
    /// Resolves a spawn prim in this (new) session: the default
    /// `/World/level/Spawn`, or — for a named `spawn` `Some("north")` — tries
    /// `/World/level/Spawn_north` first and falls back to the default. It reads
    /// that spawn's world transform and authors it onto `/World/Player` through
    /// the session-layer transform path, then re-ticks once so the ECS/render
    /// worlds reflect the move. Returns whether it placed the player.
    ///
    /// Robust by design: a missing spawn prim **or** a missing `/World/Player` is
    /// a no-op returning `false` (the player is left at the origin), not an error
    /// — a per-level assembly without an authored spawn still transitions.
    ///
    /// Authoring the spawn's **world** transform as the player's **local**
    /// transform is exact in a per-level assembly, where the player sits directly
    /// under an identity `/World` (the cook's re-rooting). `false` without the
    /// `authoring` feature (a read-only build cannot author).
    #[cfg(feature = "authoring")]
    pub fn place_player_at_spawn(&mut self, spawn: Option<&str>) -> bool {
        const DEFAULT_SPAWN: &str = "/World/level/Spawn";

        // Resolve the spawn prim path: a named spawn tries the suffixed prim
        // first, then falls back to the default spawn.
        let spawn_path = match spawn {
            Some(name) => {
                let named = format!("{DEFAULT_SPAWN}_{name}");
                if self.service.prim_at_path(&named).is_some() {
                    named
                } else {
                    DEFAULT_SPAWN.to_owned()
                }
            }
            None => DEFAULT_SPAWN.to_owned(),
        };

        // No spawn prim → leave the player where it is (not an error).
        let Some(world) = self.get_world_transform(&spawn_path) else {
            return false;
        };
        let world = world.map(|row| row.map(f64::from));
        self.author_player_world_transform(world)
    }

    /// Place the player at an explicit **world** transform (USD row-major
    /// `m[row][col]`), authored into the **session** layer. This is the
    /// level-transition `GlobalState` placement override: a one-shot pose the
    /// driver carries across a swap and applies at cutover in preference to the
    /// target level's `/World/level/Spawn`.
    ///
    /// Shares the author + re-tick tail with [`Self::place_player_at_spawn`] via
    /// [`Self::author_player_world_transform`]; the only difference is the source
    /// of the matrix (an explicit override here vs. a resolved spawn prim there).
    /// A missing `/World/Player` is a no-op returning `false`. `false` without the
    /// `authoring` feature (a read-only build cannot author).
    #[cfg(feature = "authoring")]
    pub fn place_player_at_transform(&mut self, world: [[f32; 4]; 4]) -> bool {
        let world = world.map(|row| row.map(f64::from));
        self.author_player_world_transform(world)
    }

    /// Author `world` (USD row-major `m[row][col]`) onto `/World/Player`'s
    /// session-layer transform and re-tick once so the ECS/render worlds reflect
    /// the move. Shared tail of [`Self::place_player_at_spawn`] and
    /// [`Self::place_player_at_transform`]. Returns whether it authored the pose;
    /// a missing `/World/Player` is a no-op returning `false`.
    ///
    /// `get_world_transform` returns USD row-major; the session transform author
    /// rebuilds a `GfMatrix4d` from a 16-element row-major array, so flatten
    /// directly (no transpose). With `/World` identity, the player's authored
    /// local xform then equals the given world xform.
    #[cfg(feature = "authoring")]
    fn author_player_world_transform(&mut self, world: [[f64; 4]; 4]) -> bool {
        const PLAYER: &str = "/World/Player";

        // No player to place → nothing to do.
        if self.service.prim_at_path(PLAYER).is_none() {
            return false;
        }

        let mut matrix = [0.0_f64; 16];
        for row in 0..4 {
            for col in 0..4 {
                matrix[row * 4 + col] = world[row][col];
            }
        }
        if let Err(e) = self.service.author_session_transform(PLAYER, matrix) {
            tracing::warn!("author_player_world_transform: author {PLAYER} failed: {e}");
            return false;
        }
        // Re-project once so the ECS/render worlds reflect the placement.
        if let Err(e) = self.tick() {
            tracing::warn!("author_player_world_transform: re-tick after placement failed: {e}");
        }
        true
    }

    /// Read-only-build stub: a non-`authoring` runtime cannot author the player's
    /// session-layer pose, so placement is a no-op that reports `false`.
    #[cfg(not(feature = "authoring"))]
    #[must_use]
    pub fn place_player_at_spawn(&mut self, _spawn: Option<&str>) -> bool {
        false
    }

    /// Read-only-build stub: a non-`authoring` runtime cannot author the player's
    /// session-layer pose, so the placement override is a no-op reporting `false`.
    #[cfg(not(feature = "authoring"))]
    #[must_use]
    pub fn place_player_at_transform(&mut self, _world: [[f32; 4]; 4]) -> bool {
        false
    }

    /// Returns the authored `xformOpOrder` stack for the prim at `path`.
    ///
    /// Exposed so the shell can classify the stack (canonical `rotateXYZ` TRS vs.
    /// matrix vs. unsupported) and read the raw authored Euler values for the
    /// inspector, avoiding a lossy matrix→Euler round-trip on the display path.
    /// `None` when the path is unknown or the prim is not xformable.
    #[cfg(feature = "authoring")]
    pub fn authored_xform_stack(&mut self, path: &str) -> Option<idtx_usd::AuthoredXformStack> {
        let prim_id = self.service.prim_at_path(path)?;
        self.service.authored_xform_stack(prim_id).ok()
    }

    /// Attach an **existing** Rust behavior source to the prim at `path`,
    /// authoring the `IdtxBehaviorAPI` manifest scaffold (Conductor "Attach
    /// Behavior…").
    ///
    /// The schema-attribute counterpart of the old `.wasm`-module attach: with no
    /// sidecar `idtx.toml` left in the model, the manifest now lives entirely on
    /// the prim. Applies `IdtxBehaviorAPI` (so `HasAPI` — the canonical "has a
    /// behavior" test — reports true) and authors the source triple
    /// (`idtx:behavior:source`/`:uuid`/`:kind`) plus the identity fields
    /// (`:name` + a default `:version`). The compiled `idtx:behavior:module` is
    /// **not** authored — the `.wasm` is a build artifact produced before preview
    /// / at packaging into an overlay (mirrors [`Self::create_behavior_source`]).
    ///
    /// `source_asset` is the stage-relative `.rs` path (typically
    /// `./behaviors/<stem>.rs`). Capabilities / inputs / ship / memory are left
    /// unauthored — they default sensibly and the author tunes them in the
    /// inspector's Manifest config area. A single
    /// [`SceneHydrator::reconcile_authored_behaviors`] resyncs the live
    /// `BehaviorRef`. Silently no-ops when `path` is absent.
    #[cfg(feature = "authoring")]
    pub fn attach_behavior_source(
        &mut self,
        path: &str,
        source_asset: &str,
        uuid: &str,
        kind: &str,
        name: &str,
    ) -> Result<(), SessionError> {
        use idtx_scene::behavior::attr;
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(());
        };
        // Apply the IdtxBehaviorAPI schema first so `HasAPI` (the canonical "has
        // a behavior" test) reports true and the `idtx:behavior:*` properties are
        // schema-defined members rather than free-floating customs.
        self.service
            .apply_structural_command(&idtx_usd::StructuralCommand::ApplyApi {
                path: path.to_owned(),
                schema: attr::SCHEMA.to_owned(),
            })?;
        // The manifest scaffold: the source triple (source/uuid/kind) plus the
        // identity fields (name + a default version). Capabilities / inputs / ship
        // / memory stay unauthored — they default sensibly and the author tunes
        // them in the inspector's Manifest config panel. The compiled
        // `idtx:behavior:module` is NOT authored; it is produced before preview /
        // at packaging into an overlay (mirrors `create_behavior_source`).
        for (property, value) in [
            (
                attr::SOURCE,
                idtx_usd::PropertyValue::Asset(source_asset.to_owned()),
            ),
            (attr::UUID, idtx_usd::PropertyValue::String(uuid.to_owned())),
            (attr::KIND, idtx_usd::PropertyValue::Token(kind.to_owned())),
            (attr::NAME, idtx_usd::PropertyValue::String(name.to_owned())),
            (
                attr::VERSION,
                idtx_usd::PropertyValue::String("0.1.0".to_owned()),
            ),
        ] {
            self.service
                .apply_structural_command(&idtx_usd::StructuralCommand::SetProperty {
                    prim: prim_id,
                    property: property.to_owned(),
                    value,
                })?;
        }
        self.hydrator.reconcile_authored_behaviors(&self.service);
        Ok(())
    }

    /// Detach the behavior from the prim at `path`.
    ///
    /// Removes the `IdtxBehaviorAPI` schema so `HasAPI` — the canonical "has a
    /// behavior" test — flips back to false, **and** clears every root-authored
    /// `idtx:behavior:*` opinion (module, source, kind, scope, enabled, the
    /// manifest-config token arrays, …) so a detach leaves no orphaned
    /// namespaced attributes behind. This mirrors [`Self::remove_collision_api`]:
    /// removing the schema alone would leave the authored attribute specs in the
    /// layer, and the `.wasm` module opinion in particular would keep
    /// `reconcile_authored_behaviors` seeing a live `BehaviorRef`. The whole
    /// batch is checkpointed so a mid-batch failure rolls back atomically.
    /// Silently no-ops when `path` is absent.
    ///
    /// This is a pure authoring/teardown op: the host drops the attachment on
    /// the next reconcile — playback ownership stays with the transport/session,
    /// never the inspector.
    #[cfg(feature = "authoring")]
    pub fn detach_behavior(&mut self, path: &str) -> Result<(), SessionError> {
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(());
        };

        // Gather the root-authored `idtx:behavior:*` opinions to clear alongside
        // the schema removal. Inspect before removing the schema so the authored
        // attribute specs are still listed.
        let behavior_prefix = format!("{}:", idtx_usd_extension::BEHAVIOR_NS);
        let owned: Vec<String> = self
            .service
            .inspect_prim(prim_id)?
            .into_iter()
            .filter(|r| r.root_authored && r.name.starts_with(&behavior_prefix))
            .map(|r| r.name)
            .collect();

        let checkpoint = self.service.observer_checkpoint()?;
        if let Err(e) = self.detach_behavior_batch(path, prim_id, &owned) {
            // Best-effort rollback of the partially-removed batch; surface the
            // original error.
            let _ = self.service.observer_rollback_to(checkpoint);
            return Err(e);
        }

        self.hydrator.reconcile_authored_behaviors(&self.service);
        Ok(())
    }

    /// The write batch behind [`Self::detach_behavior`]: remove the
    /// `IdtxBehaviorAPI` schema, then clear each owned root-layer
    /// `idtx:behavior:*` opinion. Factored out so the caller can roll it back
    /// atomically on any error; does **not** reconcile.
    #[cfg(feature = "authoring")]
    fn detach_behavior_batch(
        &mut self,
        path: &str,
        prim_id: idtx_usd::PrimId,
        properties: &[String],
    ) -> Result<(), SessionError> {
        self.service
            .apply_structural_command(&idtx_usd::StructuralCommand::RemoveApi {
                path: path.to_owned(),
                schema: idtx_scene::behavior::attr::SCHEMA.to_owned(),
            })?;
        // Removing the schema leaves its authored attribute specs behind, so
        // clear the root edit-target opinion of each (mirrors the collision
        // removal's `replace_root_property_text("")`).
        for property in properties {
            self.service
                .replace_root_property_text(prim_id, property, "")?;
        }
        Ok(())
    }

    /// Enable or disable an attached behavior (authors `idtx:behavior:enabled`).
    ///
    /// Disabling keeps the attachment authored but drops it from the tick loop
    /// (which fires only `enabled && kind == Runtime` behaviors), so an author
    /// can silence a behavior without detaching it. Silently no-ops when `path`
    /// is absent. Re-reads the attachment so the live `BehaviorRef` reflects the
    /// override immediately.
    #[cfg(feature = "authoring")]
    pub fn set_behavior_enabled(&mut self, path: &str, enabled: bool) -> Result<(), SessionError> {
        self.set_behavior_property(
            path,
            idtx_scene::behavior::attr::ENABLED,
            idtx_usd::PropertyValue::Bool(enabled),
        )
    }

    /// Set an attached behavior's execution kind (authors `idtx:behavior:kind`).
    #[cfg(feature = "authoring")]
    pub fn set_behavior_kind(
        &mut self,
        path: &str,
        kind: idtx_scene::BehaviorKind,
    ) -> Result<(), SessionError> {
        self.set_behavior_property(
            path,
            idtx_scene::behavior::attr::KIND,
            idtx_usd::PropertyValue::Token(kind.as_token().to_owned()),
        )
    }

    /// Set an attached behavior's binding scope (authors `idtx:behavior:scope`).
    #[cfg(feature = "authoring")]
    pub fn set_behavior_scope(
        &mut self,
        path: &str,
        scope: idtx_scene::BehaviorScope,
    ) -> Result<(), SessionError> {
        self.set_behavior_property(
            path,
            idtx_scene::behavior::attr::SCOPE,
            idtx_usd::PropertyValue::Token(scope.as_token().to_owned()),
        )
    }

    /// Opt an attached behavior into server-authoritative execution (authors
    /// `idtx:behavior:serverAuthoritative`).
    ///
    /// The authority gate ([`attach_authored_behaviors`]) attaches a behavior
    /// under a [`NetRole::Server`](idtx_scene::NetRole::Server) pawn **only** when
    /// this flag is set; it is a no-op for `Local`/`Unowned` owners (which always
    /// attach) and `Remote` owners (which never do). Round-trips through
    /// [`BehaviorRef::server_authoritative`](idtx_scene::behavior::BehaviorRef)
    /// on the next reconcile.
    #[cfg(feature = "authoring")]
    pub fn set_behavior_server_authoritative(
        &mut self,
        path: &str,
        server_authoritative: bool,
    ) -> Result<(), SessionError> {
        self.set_behavior_property(
            path,
            idtx_scene::behavior::attr::SERVER_AUTHORITATIVE,
            idtx_usd::PropertyValue::Bool(server_authoritative),
        )
    }

    /// Author an arbitrary `IdtxBehaviorAPI` config attribute on the behavior at
    /// `path` (capabilities / inputs token arrays, version, ship, memory cap),
    /// reconciling the live `BehaviorRef`. The generic entry behind the
    /// inspector's behavior-config area; routes through the same session-layer
    /// override path as the typed setters.
    #[cfg(feature = "authoring")]
    pub fn set_behavior_attribute(
        &mut self,
        path: &str,
        property: &str,
        value: idtx_usd::PropertyValue,
    ) -> Result<(), SessionError> {
        self.set_behavior_property(path, property, value)
    }

    /// Author a single per-attachment override attribute and reconcile the live
    /// `BehaviorRef` so the change is reflected without a topology re-projection.
    ///
    /// Behavior override attributes carry no viewport payload, so authoring one
    /// fires only a value-only notice; we therefore resync the attachment
    /// explicitly via [`SceneHydrator::reconcile_authored_behaviors`] rather
    /// than waiting for a topology drain (which never comes). No-ops when `path`
    /// is absent.
    #[cfg(feature = "authoring")]
    fn set_behavior_property(
        &mut self,
        path: &str,
        property: &str,
        value: idtx_usd::PropertyValue,
    ) -> Result<(), SessionError> {
        self.author_attribute(path, property, value)?;
        self.hydrator.reconcile_authored_behaviors(&self.service);
        Ok(())
    }

    /// Create a **source-authored** behavior on the prim at `path` (Conductor
    /// "Create Behavior").
    ///
    /// Applies the `IdtxBehaviorAPI` schema and authors the *source* half of the
    /// behavior — `idtx:behavior:source` (the editor-facing `.rs` the author
    /// edits), `idtx:behavior:uuid`, and `idtx:behavior:kind` — but **not**
    /// `idtx:behavior:module` (the compiled `.wasm`). No module exists yet: the
    /// `.rs` is cross-compiled to `.wasm` only before preview / at packaging, and
    /// the compiled module is linked in the preview/cook overlay so this source
    /// stage stays source-only. (`HasAPI(IdtxBehaviorAPI)` still reports true —
    /// the prim *has* a behavior — even though it will not yet tick.)
    ///
    /// `source_asset` is the `.rs` path to author (typically `./behaviors/<stem>.rs`,
    /// relative to the stage). All three attributes are authored in one structural
    /// pass, then a single [`SceneHydrator::reconcile_authored_behaviors`] resyncs
    /// the live `BehaviorRef` (which will carry `source_asset = Some(..)` and
    /// `module_asset = None`). Silently no-ops when `path` is absent.
    #[cfg(feature = "authoring")]
    pub fn create_behavior_source(
        &mut self,
        path: &str,
        uuid: &str,
        kind: &str,
        source_asset: &str,
    ) -> Result<(), SessionError> {
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(());
        };
        // Apply the IdtxBehaviorAPI schema first so `HasAPI` (the canonical "has
        // a behavior" test) reports true and the `idtx:behavior:*` properties are
        // schema-defined members rather than free-floating customs.
        self.service
            .apply_structural_command(&idtx_usd::StructuralCommand::ApplyApi {
                path: path.to_owned(),
                schema: idtx_scene::behavior::attr::SCHEMA.to_owned(),
            })?;
        for (property, value) in [
            (
                idtx_scene::behavior::attr::SOURCE,
                idtx_usd::PropertyValue::Asset(source_asset.to_owned()),
            ),
            (
                idtx_scene::behavior::attr::UUID,
                idtx_usd::PropertyValue::String(uuid.to_owned()),
            ),
            (
                idtx_scene::behavior::attr::KIND,
                idtx_usd::PropertyValue::Token(kind.to_owned()),
            ),
        ] {
            self.service
                .apply_structural_command(&idtx_usd::StructuralCommand::SetProperty {
                    prim: prim_id,
                    property: property.to_owned(),
                    value,
                })?;
        }
        self.hydrator.reconcile_authored_behaviors(&self.service);
        Ok(())
    }

    /// Ensure the stage-level behavior prim [`STAGE_BEHAVIOR_PRIM_PATH`] exists
    /// and the root layer names it in `customLayerData["idtx:stageBehavior"]`.
    ///
    /// Defines `/StageBehavior` as a `def Scope` when absent (it carries no
    /// geometry — only `IdtxBehaviorAPI`), then authors the root-layer metadata:
    /// the prim name plus the run-condition token (preserving an already-authored
    /// run-scope, else defaulting to [`StageBehaviorRunScope::Standalone`]). This
    /// is the stage-specific preamble the shell runs *before* the normal behavior
    /// source scaffolding ([`Self::create_behavior_source`] /
    /// [`Self::attach_behavior_source`] on `/StageBehavior`), which applies the
    /// schema and authors the source triple. The scope token is set to `stage`
    /// afterwards by the caller via [`Self::set_behavior_scope`].
    #[cfg(feature = "authoring")]
    pub fn ensure_stage_behavior_prim(&mut self) -> Result<(), SessionError> {
        if self.service.prim_at_path(STAGE_BEHAVIOR_PRIM_PATH).is_none() {
            self.define_prim(STAGE_BEHAVIOR_PRIM_PATH, "Scope")?;
        }
        let run_scope = self.stage_behavior_run_scope;
        self.service
            .author_root_stage_behavior(STAGE_BEHAVIOR_NAME, run_scope)?;
        Ok(())
    }

    /// The stage behavior's live run-condition (seeded from root-layer metadata
    /// at open, updated by [`Self::set_stage_behavior_run_scope`] /
    /// [`Self::ensure_stage_behavior_prim`]). The UI + run-gate source of truth.
    #[must_use]
    pub fn stage_behavior_run_scope(&self) -> idtx_usd::StageBehaviorRunScope {
        self.stage_behavior_run_scope
    }

    /// The stage behavior's run scope read from the layer that actually *owns* it
    /// (the one carrying `idtx:stageBehavior`) — the value the attach/tick gate
    /// enforces. `None` when no owning-layer scope is authored, so a caller can
    /// fall back to [`stage_behavior_run_scope`](Self::stage_behavior_run_scope)
    /// (correct for a directly-opened stage). The UI uses this so its display and
    /// re-authoring match the enforced scope rather than the root-only cache.
    #[must_use]
    pub fn stage_behavior_run_scope_resolved(&self) -> Option<idtx_usd::StageBehaviorRunScope> {
        self.service.stage_behavior_run_scope_resolved()
    }

    /// Whether a stage-scoped behavior may attach/tick in this stage's current
    /// composition: its run scope — resolved from the layer that *owns* the
    /// behavior (the payloaded level), falling back to the cached metadata copy
    /// for a directly-opened stage — [`permits`](idtx_usd::StageBehaviorRunScope::permits)
    /// the stage's composition context. The shared attach-time gate for both the
    /// session and the standalone-runtime attach paths.
    #[must_use]
    pub fn stage_behavior_permitted(&self) -> bool {
        let scope = self
            .service
            .stage_behavior_run_scope_resolved()
            .unwrap_or(self.stage_behavior_run_scope);
        scope.permits(self.service.stage_behavior_compose_context())
    }

    /// `true` when a stage-level behavior prim exists in the stage.
    #[must_use]
    pub fn has_stage_behavior(&self) -> bool {
        self.service.prim_at_path(STAGE_BEHAVIOR_PRIM_PATH).is_some()
    }

    /// Re-author the stage behavior's run-condition token (keeping the prim
    /// name) and update the live copy.
    #[cfg(feature = "authoring")]
    pub fn set_stage_behavior_run_scope(
        &mut self,
        scope: idtx_usd::StageBehaviorRunScope,
    ) -> Result<(), SessionError> {
        self.service
            .author_root_stage_behavior(STAGE_BEHAVIOR_NAME, scope)?;
        self.stage_behavior_run_scope = scope;
        Ok(())
    }

    /// Remove the stage-level behavior entirely: detach the behavior (clears the
    /// `IdtxBehaviorAPI` schema + every `idtx:behavior:*` opinion), delete the
    /// `/StageBehavior` prim, and erase the root-layer `customLayerData` keys.
    /// Silently no-ops when no stage behavior prim exists.
    #[cfg(feature = "authoring")]
    pub fn remove_stage_behavior(&mut self) -> Result<(), SessionError> {
        if self.service.prim_at_path(STAGE_BEHAVIOR_PRIM_PATH).is_some() {
            self.detach_behavior(STAGE_BEHAVIOR_PRIM_PATH)?;
            self.delete_prim(STAGE_BEHAVIOR_PRIM_PATH)?;
        }
        self.service.clear_root_stage_behavior()?;
        // Reset the cached run scope to the default now the root-layer metadata is
        // gone. Otherwise a later `ensure_stage_behavior_prim` would re-author a
        // fresh behavior with the *removed* behavior's run scope (the cache is the
        // source of truth it reads), and the Stage panel would show the stale
        // scope.
        self.stage_behavior_run_scope = idtx_usd::StageBehaviorRunScope::default();
        Ok(())
    }
    ///
    /// Deliberately omits `module`/`source`/`uuid`: the overwrite regenerates a
    /// fresh source file with a new uuid, so only the manifest-config survives.
    /// Each field is `None`/empty when its attribute is absent, so a sparse
    /// behavior copies over only what was authored. Reads composed values (any
    /// layer), reusing the same typed readers as [`manifest_from_schema`].
    /// Returns `None` only when `path` resolves to no prim.
    #[cfg(feature = "authoring")]
    #[must_use]
    pub fn read_composed_behavior_config(&self, path: &str) -> Option<BehaviorConfigSnapshot> {
        use idtx_scene::behavior::attr;
        let prim = self.service.prim_at_path(path)?;
        Some(BehaviorConfigSnapshot {
            kind: read_text_attr(&self.service, prim, attr::KIND),
            scope: read_text_attr(&self.service, prim, attr::SCOPE),
            enabled: read_bool_attr(&self.service, prim, attr::ENABLED),
            server_authoritative: read_bool_attr(&self.service, prim, attr::SERVER_AUTHORITATIVE),
            capabilities: self
                .service
                .eval_token_array(prim, attr::CAPABILITIES, 0.0, true)
                .unwrap_or_default(),
            inputs: self
                .service
                .eval_token_array(prim, attr::INPUTS, 0.0, true)
                .unwrap_or_default(),
            version: read_text_attr(&self.service, prim, attr::VERSION),
            ship: read_bool_attr(&self.service, prim, attr::SHIP),
            memory_mb: read_int_attr(&self.service, prim, attr::MEMORY_MB),
        })
    }

    /// Author a [`BehaviorConfigSnapshot`] onto the prim at `path` as strong
    /// opinions in the current edit layer — the write half of
    /// `overwrite_behavior` that carries a weaker-layer behavior's manifest into
    /// the local `over`. Each present field routes through the same per-attribute
    /// override path as the inspector's config setters
    /// ([`Self::set_behavior_attribute`]); absent fields are left untouched
    /// (falling back to the freshly-created source's defaults). No-ops when
    /// `path` is absent.
    #[cfg(feature = "authoring")]
    pub fn apply_behavior_config(
        &mut self,
        path: &str,
        cfg: &BehaviorConfigSnapshot,
    ) -> Result<(), SessionError> {
        use idtx_scene::behavior::attr;
        use idtx_usd::PropertyValue as PV;
        if let Some(kind) = &cfg.kind {
            self.set_behavior_attribute(path, attr::KIND, PV::Token(kind.clone()))?;
        }
        if let Some(scope) = &cfg.scope {
            self.set_behavior_attribute(path, attr::SCOPE, PV::Token(scope.clone()))?;
        }
        if let Some(enabled) = cfg.enabled {
            self.set_behavior_attribute(path, attr::ENABLED, PV::Bool(enabled))?;
        }
        if let Some(server_authoritative) = cfg.server_authoritative {
            self.set_behavior_attribute(
                path,
                attr::SERVER_AUTHORITATIVE,
                PV::Bool(server_authoritative),
            )?;
        }
        if !cfg.capabilities.is_empty() {
            self.set_behavior_attribute(
                path,
                attr::CAPABILITIES,
                PV::TokenArray(cfg.capabilities.clone()),
            )?;
        }
        if !cfg.inputs.is_empty() {
            self.set_behavior_attribute(path, attr::INPUTS, PV::TokenArray(cfg.inputs.clone()))?;
        }
        if let Some(version) = &cfg.version {
            self.set_behavior_attribute(path, attr::VERSION, PV::String(version.clone()))?;
        }
        if let Some(ship) = cfg.ship {
            self.set_behavior_attribute(path, attr::SHIP, PV::Bool(ship))?;
        }
        if let Some(mb) = cfg.memory_mb
            && let Ok(mb) = i32::try_from(mb)
        {
            self.set_behavior_attribute(path, attr::MEMORY_MB, PV::Int(mb))?;
        }
        Ok(())
    }

    /// Read the authored `idtx:behavior:source` (`.rs`) path for the prim at
    /// `path`, or `None` when no non-empty source is authored.
    ///
    /// Recovers the path from USD's `@<path>@`-delimited `SdfAssetPath` summary;
    /// an empty asset reads as absent (mirroring the detach convention on the
    /// module attribute). Not authoring-gated — the Conductor's source-first
    /// Edit/Check resolution reads it in every mode.
    pub fn behavior_source_path(&mut self, path: &str) -> Option<String> {
        let props = self.inspect_prim(path).ok()?;
        let rec = props
            .into_iter()
            .find(|p| p.name == idtx_scene::behavior::attr::SOURCE && p.authored)?;
        let s = rec.value_summary.trim();
        let inner = match (s.find('@'), s.rfind('@')) {
            (Some(first), Some(last)) if last > first => &s[first + 1..last],
            _ => s,
        }
        .trim();
        (!inner.is_empty()).then(|| inner.to_owned())
    }

    /// Resolve the authored `idtx:behavior:source` (`.rs`) on the prim at
    /// `prim_path` to an absolute filesystem path through USD's asset resolver —
    /// the **same** resolution preview/packaging use
    /// (`behavior_prepare::prepare` via [`StageService::resolve_prim_asset`]).
    ///
    /// The authored path is anchored against its authoring layer **and** the
    /// project's installed resolver search path, so a search-path-anchored
    /// `@assets/behaviors/x.rs@` resolves to `<project>/assets/behaviors/x.rs`
    /// rather than a naive join onto the stage directory (the bug the Conductor
    /// Check/Verify buttons hit). Returns `None` when the prim is absent, the
    /// attribute is unauthored / not an asset, or it does not resolve.
    ///
    /// Any Windows verbatim (`\\?\`) prefix is stripped (via
    /// [`idtx_project::strip_verbatim_prefix`]) so the returned path is a plain,
    /// cross-platform value safe to display and to hand to `cargo`.
    #[must_use]
    pub fn resolve_behavior_source_path(&self, prim_path: &str) -> Option<std::path::PathBuf> {
        let prim = self.service.prim_at_path(prim_path)?;
        let resolved = self
            .service
            .resolve_prim_asset(prim, idtx_scene::behavior::attr::SOURCE)
            .ok()
            .flatten()?;
        Some(idtx_project::strip_verbatim_prefix(std::path::Path::new(
            &resolved,
        )))
    }

    /// `true` when the prim at `path` has the `IdtxBehaviorAPI` schema applied —
    /// the canonical "this prim has a behavior" test. Covers both a
    /// compiled-module attachment and a source-only (not-yet-compiled) behavior,
    /// since both apply the schema. `false` when `path` is absent or the query
    /// fails.
    pub fn prim_has_behavior(&mut self, path: &str) -> bool {
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return false;
        };
        self.service
            .prim_has_api(prim_id, idtx_scene::behavior::attr::SCHEMA)
            .unwrap_or(false)
    }

    /// `StructuralCommand::CreateOverride`. Only the path crosses the bridge —
    /// no array payload.
    ///
    /// An *inert* (empty) over changes no composed value, so USD fires only an
    /// info-only `ObjectsChanged` notice (no `TOPOLOGY`/resync) that `tick()`
    /// routes down the value-only branch — which bumps `change_generation` but
    /// *not* `topology_generation`. We therefore reproject the affected paths
    /// explicitly (so the cached topology provenance, and thus
    /// `is_locally_authored`, refreshes immediately) and advance
    /// `topology_generation` ourselves. The latter is what forces the shell to
    /// rebuild its cached scene-tree nodes and Layer Stack view — both keyed
    /// solely off `topology_generation` — so the tree row flips from
    /// display-only to editable and its context menu updates in lockstep with
    /// the inspector and gizmo (which also refresh on `change_generation`).
    ///
    /// The reprojection covers the *whole ancestor chain*, not just `path`:
    /// `SdfCreatePrimInLayer` also authors fresh overs on any parent prim that
    /// had no root-layer spec, and those parents' provenance must refresh too —
    /// see [`ancestor_chain_paths`].
    ///
    /// Silently no-ops when `path` is not present in the current stage.
    #[cfg(feature = "authoring")]
    pub fn create_opinion(&mut self, path: &str) -> Result<(), SessionError> {
        if self.service.prim_at_path(path).is_none() {
            return Ok(());
        }
        self.service
            .apply_structural_command(&idtx_usd::StructuralCommand::CreateOverride {
                path: path.to_owned(),
            })?;
        // `SdfCreatePrimInLayer` authors an `over` for the target prim *and any
        // missing ancestor prims* along its path (e.g. authoring an opinion on
        // a cube nested three payloads deep also creates fresh overs on every
        // parent that had no root-layer spec yet). Reproject the *whole ancestor
        // chain*, not just the leaf: a per-leaf reprojection would leave each
        // parent's cached provenance stale, so the scene tree would keep drawing
        // them display-only (grey) instead of flipping to editable/`over`
        // (orange) in lockstep with the leaf.
        let roots = ancestor_chain_paths(path);
        self.service.project_topology_paths(&roots)?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        Ok(())
    }

    /// Remove an empty root-layer `over` previously authored by
    /// [`Self::create_opinion`]. Errors (does not no-op) if the prim carries
    /// authored data or has no removable override, so callers surface the reason.
    ///
    /// Like [`Self::create_opinion`], removing an inert over emits only an
    /// info-only notice, so the affected path is reprojected explicitly and
    /// `topology_generation` is advanced to force the shell to rebuild its
    /// cached scene-tree nodes (and Layer Stack). Without the bump the tree
    /// would keep painting the row as editable — with a stale "Remove Opinion"
    /// context menu — even after the inspector and gizmo have reverted to
    /// display-only.
    #[cfg(feature = "authoring")]
    pub fn remove_opinion(&mut self, path: &str) -> Result<(), SessionError> {
        self.service
            .apply_structural_command(&idtx_usd::StructuralCommand::RemoveOverride {
                path: path.to_owned(),
            })?;
        self.service.project_topology_paths(&[path.to_owned()])?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        Ok(())
    }

    /// Author a new typed prim at `path` into the current edit target.
    ///
    /// `type_name` is the USD prim type (e.g. `"Xform"`, `"Scope"`, `"Mesh"`);
    /// an empty string authors a typeless prim. Like the other structural
    /// authoring helpers this reprojects the whole topology (the new prim plus
    /// any freshly-authored ancestor overs) and advances `topology_generation`
    /// so the shell rebuilds its cached scene-tree nodes.
    #[cfg(feature = "authoring")]
    pub fn define_prim(&mut self, path: &str, type_name: &str) -> Result<(), SessionError> {
        self.service
            .apply_structural_command(&idtx_usd::StructuralCommand::Define {
                path: path.to_owned(),
                type_name: type_name.to_owned(),
            })?;
        self.service.project_topology()?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        Ok(())
    }

    /// Author a new typed child under `parent_path`, choosing a unique name
    /// derived from `type_name` (e.g. `Xform`, `Xform1`, …). Returns the full
    /// path of the created prim so the caller can select it.
    ///
    /// No-ops with `Ok(None)` when `parent_path` does not resolve in the
    /// current stage.
    #[cfg(feature = "authoring")]
    pub fn define_child(
        &mut self,
        parent_path: &str,
        type_name: &str,
    ) -> Result<Option<String>, SessionError> {
        if self.service.prim_at_path(parent_path).is_none() {
            return Ok(None);
        }
        // Base the display name on the type; fall back to "Prim" for typeless.
        let base = if type_name.is_empty() {
            "Prim"
        } else {
            type_name
        };
        let parent = parent_path.trim_end_matches('/');
        // Find the lowest free "<base>", "<base>1", "<base>2", … under parent.
        let mut candidate = format!("{parent}/{base}");
        let mut suffix = 1u32;
        while self.service.prim_at_path(&candidate).is_some() {
            candidate = format!("{parent}/{base}{suffix}");
            suffix += 1;
        }
        self.define_prim(&candidate, type_name)?;
        Ok(Some(candidate))
    }

    /// Author a typed child prim *with schema-sensible default attributes* under
    /// `parent_path` (camera / light / environment), choosing a unique
    /// name derived from the template's prim type (e.g. `Camera`, `Camera1`, …).
    /// Returns the full path of the created prim so the caller can select it.
    ///
    /// No-ops with `Ok(None)` when `parent_path` does not resolve.
    #[cfg(feature = "authoring")]
    pub fn define_child_from_template(
        &mut self,
        parent_path: &str,
        template: idtx_usd::PrimTemplate,
    ) -> Result<Option<String>, SessionError> {
        if self.service.prim_at_path(parent_path).is_none() {
            return Ok(None);
        }
        let base = template.type_name();
        let parent = parent_path.trim_end_matches('/');
        // Find the lowest free "<base>", "<base>1", "<base>2", … under parent.
        let mut candidate = format!("{parent}/{base}");
        let mut suffix = 1u32;
        while self.service.prim_at_path(&candidate).is_some() {
            candidate = format!("{parent}/{base}{suffix}");
            suffix += 1;
        }
        self.service
            .define_prim_from_template(&candidate, template)?;
        self.service.project_topology()?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        Ok(Some(candidate))
    }

    /// Capture the current root layer as USDA text — a full snapshot suitable
    /// for structural undo/redo.
    ///
    /// Structural edits (define, delete, add reference/payload, rename,
    /// reparent) all author into the root layer, so a `before`/`after` pair of
    /// these snapshots forms a reversible record: restore `before` to undo,
    /// `after` to redo. See [`restore_root_layer_snapshot`][Self::restore_root_layer_snapshot].
    #[cfg(feature = "authoring")]
    pub fn root_layer_snapshot(&self) -> Result<String, SessionError> {
        Ok(self.service.root_layer_text()?)
    }

    /// Restore a previously-captured [`root_layer_snapshot`][Self::root_layer_snapshot],
    /// reproject topology, and advance `topology_generation` so the shell
    /// rebuilds its cached scene tree.
    ///
    /// This is the authoring primitive behind `EditCommand::Structural`
    /// undo/redo: the shell restores the command's `before` (undo) or `after`
    /// (redo) text.
    #[cfg(feature = "authoring")]
    pub fn restore_root_layer_snapshot(&mut self, source: &str) -> Result<(), SessionError> {
        self.service.replace_root_layer_text(source)?;
        self.refresh_after_root_edit()
    }

    #[cfg(feature = "authoring")]
    pub fn apply_root_layer_delta(&mut self, expected: &str, desired: &str) -> Result<(), SessionError> {
        #[cfg(feature = "metrics")]
        let started = std::time::Instant::now();
        self.service.apply_root_layer_delta(expected, desired)?;
        let result = self.refresh_after_root_edit();
        #[cfg(feature = "metrics")]
        tracing::debug!(elapsed_us = started.elapsed().as_micros() as u64, snapshot_bytes = expected.len() + desired.len(), "undo/redo root delta");
        result
    }

    #[cfg(feature = "authoring")]
    pub fn root_transform_snapshot(&self, path: &str) -> Result<String, SessionError> {
        Ok(self.service.root_transform_text(path)?)
    }

    #[cfg(feature = "authoring")]
    pub fn replay_root_transform(&mut self, path: &str, expected: &str, desired: &str) -> Result<(), SessionError> {
        if self.service.prim_at_path(path).is_none() { return Err(StageError::UnknownPath(path.into()).into()); }
        self.apply_root_layer_delta(expected, desired)?;
        #[cfg(feature = "remote")]
        {
            self.publish_local_transform(path)?;
            // The wire carries values, not absence of the authored op stack.
            if !desired.contains("xformOpOrder") && let Some(state) = &mut self.collab_persistence { state.local_only_dirty = true; }
        }
        Ok(())
    }

    #[cfg(feature = "authoring")]
    fn refresh_after_root_edit(&mut self) -> Result<(), SessionError> {
        let delta = self.service.project_topology_paths(&[])?;
        self.hydrator.apply_topology_delta(&delta, &self.service);
        self.stage_shadow_mode = self.service.reread_shadow_mode()?;
        self.snapshot_dirty = true;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        // A snapshot swap can add or drop an authored `idtx:behavior:module` attachment
        // without any topology change (attach/detach only touches an attribute),
        // and the value-only drain path never re-hydrates behaviors — so an
        // undone/redone attach would otherwise leave a stale live `BehaviorRef`.
        // Reconcile the ECS refs against the freshly-restored attributes.
        self.hydrator.reconcile_authored_behaviors(&self.service);
        // A snapshot swap can likewise add/drop an authored `idtx:playerRole`;
        // `hydrate_player_roles` is written to also remove a stale role, so it
        // doubles as the reconcile path here.
        self.hydrator.hydrate_player_roles(&self.service);
        // Likewise reconcile CollisionComp: a snapshot swap / undo-redo can add
        // or drop the IdtxCollisionAPI; hydrate_collision removes a stale one.
        self.hydrator.hydrate_collision(&self.service);
        // Reconcile HudComp the same way: a session-layer HUD prim added/removed
        // by a behavior, or an undo/redo, must be reflected in the ECS.
        self.hydrator.hydrate_hud(&self.service);
        self.hydrator.hydrate_widgets(&self.service);
        // Resync the cached stage-behavior run scope. A snapshot swap can restore a
        // different `idtx:stageBehaviorRunScope` in the root layer's
        // `customLayerData`; the cached copy is the UI + run-gate source of truth
        // (like `stage_shadow_mode`), so leaving it at the pre-undo value would make
        // the Stage panel display — and re-author from — stale state. Re-read it
        // live from the freshly-restored root layer.
        self.stage_behavior_run_scope = self
            .service
            .reread_stage_behavior_run_scope()
            .unwrap_or_default();
        Ok(())
    }

    /// Delete the prim at `path` (and its subtree) from the current edit
    /// target. No-ops when `path` does not resolve.
    #[cfg(feature = "authoring")]
    pub fn delete_prim(&mut self, path: &str) -> Result<(), SessionError> {
        let Some(prim) = self.service.prim_at_path(path) else {
            return Ok(());
        };
        self.service
            .apply_structural_command(&idtx_usd::StructuralCommand::Delete { prim })?;
        self.service.project_topology()?;
        // The full reprojection above already dropped the deleted subtree from
        // `service.topology()`, so the next `tick()`'s `project_topology_paths`
        // diff has no baseline to compare against and never reports the prims as
        // removed — leaving their ECS entities (and any `LightComp` / `MeshComp`)
        // alive to feed stale data into the render extract (a deleted light would
        // keep lighting the scene). Despawn the subtree explicitly and force a
        // fresh snapshot so the removal is reflected immediately.
        self.hydrator.despawn_subtree(path);
        self.snapshot_dirty = true;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        Ok(())
    }

    // ── Stage-level physics authoring ─────────────────────────────────────────

    /// Read the stage's current `UsdPhysicsScene` state for the Stage inspector
    /// panel: whether a `PhysicsScene`-typed prim is present (which gates runtime
    /// simulation) and its authored gravity. Scans the hydrated ECS for the first
    /// `PhysicsScene` prim; returns the schema defaults when none is present.
    #[cfg(feature = "authoring")]
    #[must_use]
    pub fn physics_scene_state(&self) -> PhysicsSceneState {
        let mut state = PhysicsSceneState::default();
        let Some(prim) = self.physics_scene_prim() else {
            return state;
        };
        state.active = true;
        if let Some(dir) = self
            .service
            .eval_attribute(prim, idtx_usd_extension::ATTR_PHYSICS_GRAVITY_DIRECTION, 0.0, true)
            .ok()
            .flatten()
            .and_then(|v| parse_vec3_repr(&v.value_repr))
        {
            state.gravity_direction = dir;
        }
        if let Some(mag) = self
            .service
            .eval_attribute(prim, idtx_usd_extension::ATTR_PHYSICS_GRAVITY_MAGNITUDE, 0.0, true)
            .ok()
            .flatten()
            .and_then(|v| v.value_repr.trim().parse::<f32>().ok())
        {
            state.gravity_magnitude = mag;
        }
        state
    }

    /// The hydrated path of the first `PhysicsScene`-typed prim, if any.
    #[cfg(feature = "authoring")]
    #[must_use]
    pub fn physics_scene_path(&self) -> Option<String> {
        let prim = self.physics_scene_prim()?;
        self.service.path_for(prim).map(str::to_owned)
    }

    /// The [`PrimId`] of the first `PhysicsScene`-typed prim in the composed
    /// stage. Reads the service topology (updated immediately by an authoring
    /// reprojection) rather than the ECS (re-hydrated only on the next `tick`),
    /// so a just-authored scene is detected within the same call.
    #[cfg(feature = "authoring")]
    fn physics_scene_prim(&self) -> Option<idtx_usd::PrimId> {
        self.service
            .topology()
            .iter()
            .find(|r| r.type_name == idtx_usd_extension::PHYSICS_SCENE_TYPE)
            .map(|r| r.prim)
    }

    /// Activate runtime physics by authoring a `PhysicsScene` prim named
    /// `physicsScene` as a child of the stage pseudo-root, carrying the schema
    /// defaults `physics:gravityDirection = (0, -1, 0)` and an earth-gravity
    /// `physics:gravityMagnitude` **expressed in stage units/s²** — i.e.
    /// `9.81 / metersPerUnit`, so a centimetre stage (`metersPerUnit = 0.01`)
    /// receives `981`, not `9.81` (which would simulate 100× too slowly). The
    /// runtime treats an explicit positive magnitude as stage units verbatim, so
    /// the unit scaling must happen here at author time. No-ops when a
    /// `PhysicsScene` prim is already present.
    #[cfg(feature = "authoring")]
    pub fn activate_physics(&mut self) -> Result<(), SessionError> {
        use idtx_usd::{PropertyValue, StructuralCommand};
        if self.physics_scene_prim().is_some() {
            return Ok(());
        }
        // Earth gravity in stage units/s²: 9.81 m/s² ÷ metersPerUnit. (The literal
        // mirrors `idtx_physics::scene::EARTH_GRAVITY`, which is unavailable in an
        // authoring-only build without the `physics` feature.)
        #[allow(clippy::cast_possible_truncation)]
        let mpu = {
            let m = self.service.metadata().meters_per_unit;
            if m > 0.0 { m as f32 } else { 1.0 }
        };
        let gravity_magnitude = 9.81_f32 / mpu;
        let path = PHYSICS_SCENE_DEFAULT_PATH;
        self.service.apply_structural_command(&StructuralCommand::Define {
            path: path.to_owned(),
            type_name: idtx_usd_extension::PHYSICS_SCENE_TYPE.to_owned(),
        })?;
        self.service.project_topology()?;
        if let Some(prim) = self.service.prim_at_path(path) {
            self.service.apply_structural_command(&StructuralCommand::SetProperty {
                prim,
                property: idtx_usd_extension::ATTR_PHYSICS_GRAVITY_DIRECTION.to_owned(),
                value: PropertyValue::Vector3f([0.0, -1.0, 0.0]),
            })?;
            self.service.apply_structural_command(&StructuralCommand::SetProperty {
                prim,
                property: idtx_usd_extension::ATTR_PHYSICS_GRAVITY_MAGNITUDE.to_owned(),
                value: PropertyValue::F32(gravity_magnitude),
            })?;
            self.service.project_topology()?;
        }
        self.topology_generation = self.topology_generation.wrapping_add(1);
        Ok(())
    }

    /// Deactivate runtime physics by deleting the stage's `PhysicsScene` prim.
    /// No-ops when none is present.
    #[cfg(feature = "authoring")]
    pub fn deactivate_physics(&mut self) -> Result<(), SessionError> {
        let Some(path) = self.physics_scene_path() else {
            return Ok(());
        };
        self.delete_prim(&path)
    }

    /// Author `physics:gravityDirection` onto the stage's `PhysicsScene` prim.
    /// No-ops when physics is not active.
    #[cfg(feature = "authoring")]
    pub fn set_physics_gravity_direction(
        &mut self,
        dir: [f32; 3],
    ) -> Result<(), SessionError> {
        self.set_physics_scene_property(
            idtx_usd_extension::ATTR_PHYSICS_GRAVITY_DIRECTION,
            idtx_usd::PropertyValue::Vector3f(dir),
        )
    }

    /// Author `physics:gravityMagnitude` onto the stage's `PhysicsScene` prim.
    /// No-ops when physics is not active.
    #[cfg(feature = "authoring")]
    pub fn set_physics_gravity_magnitude(&mut self, mag: f32) -> Result<(), SessionError> {
        self.set_physics_scene_property(
            idtx_usd_extension::ATTR_PHYSICS_GRAVITY_MAGNITUDE,
            idtx_usd::PropertyValue::F32(mag),
        )
    }

    /// Author a single property onto the stage's `PhysicsScene` prim and reproject
    /// its subtree. No-ops when no `PhysicsScene` prim is present.
    #[cfg(feature = "authoring")]
    fn set_physics_scene_property(
        &mut self,
        property: &str,
        value: idtx_usd::PropertyValue,
    ) -> Result<(), SessionError> {
        let Some(prim) = self.physics_scene_prim() else {
            return Ok(());
        };
        let path = self.physics_scene_path();
        self.service
            .apply_structural_command(&idtx_usd::StructuralCommand::SetProperty {
                prim,
                property: property.to_owned(),
                value,
            })?;
        if let Some(path) = path {
            self.service.project_topology_paths(&[path])?;
        }
        Ok(())
    }

    #[cfg(feature = "authoring")]
    pub fn rename_prim(&mut self, path: &str, new_name: &str) -> Result<(), SessionError> {
        let Some(prim) = self.service.prim_at_path(path) else {
            return Ok(());
        };
        self.service
            .apply_structural_command(&idtx_usd::StructuralCommand::Rename {
                prim,
                new_name: new_name.to_owned(),
            })?;
        self.service.project_topology()?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        Ok(())
    }

    /// Reparent the prim at `path` under `new_parent_path`, moving its whole
    /// subtree. No-ops when either path does not resolve.
    #[cfg(feature = "authoring")]
    pub fn reparent_prim(&mut self, path: &str, new_parent_path: &str) -> Result<(), SessionError> {
        let (Some(prim), Some(new_parent)) = (
            self.service.prim_at_path(path),
            self.service.prim_at_path(new_parent_path),
        ) else {
            return Ok(());
        };
        self.service
            .apply_structural_command(&idtx_usd::StructuralCommand::Reparent {
                prim,
                new_parent,
            })?;
        self.service.project_topology()?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        Ok(())
    }

    /// Add a composition reference on the prim at `path` to an external
    /// `asset_path`. When `target_path` is `Some(absolute_prim_path)` the arc
    /// targets that prim (`@asset@</Prim>`); `None` composes the asset's
    /// `defaultPrim`. No-ops when `path` does not resolve.
    #[cfg(feature = "authoring")]
    pub fn add_reference(
        &mut self,
        path: &str,
        asset_path: &str,
        target_path: Option<&str>,
    ) -> Result<(), SessionError> {
        let Some(prim) = self.service.prim_at_path(path) else {
            return Ok(());
        };
        self.service
            .apply_structural_command(&idtx_usd::StructuralCommand::AddReference {
                prim,
                asset_path: asset_path.to_owned(),
                target_path: target_path.map(str::to_owned),
            })?;
        self.service.project_topology()?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        Ok(())
    }

    /// Add a payload arc on the prim at `path` to an external `asset_path`
    /// (deferred-load composition). When `target_path` is
    /// `Some(absolute_prim_path)` the arc targets that prim (`@asset@</Prim>`);
    /// `None` composes the asset's `defaultPrim`. No-ops when `path` does not
    /// resolve.
    #[cfg(feature = "authoring")]
    pub fn add_payload(
        &mut self,
        path: &str,
        asset_path: &str,
        target_path: Option<&str>,
    ) -> Result<(), SessionError> {
        let Some(prim) = self.service.prim_at_path(path) else {
            return Ok(());
        };
        self.service
            .apply_structural_command(&idtx_usd::StructuralCommand::AddPayload {
                prim,
                asset_path: asset_path.to_owned(),
                target_path: target_path.map(str::to_owned),
            })?;
        self.service.project_topology()?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        Ok(())
    }

    /// Author a new `Xform` child under the prim at `parent_path` and attach an
    /// asset-composition arc to it — a **payload** by default, or a
    /// **reference** when `as_reference` is `true`. The child is named after the
    /// asset's file stem, deduped against existing siblings (`Name`, `Name1`,
    /// …). `target_prim` names the absolute prim path the arc should point at
    /// (`@asset@</Prim>`); pass `None` to compose the asset's `defaultPrim`.
    /// Returns the created child's path so the caller can select it, or
    /// `Ok(None)` when `parent_path` does not resolve.
    ///
    /// This is the scene-tree drag-drop authoring path: dropping a USD-stage
    /// asset onto a prim *nests* the asset under it (a new child carrying the
    /// arc), rather than composing the arc directly onto the drop-target prim
    /// (which is what [`add_reference`][Self::add_reference] /
    /// [`add_payload`][Self::add_payload] do). Both the `Define` and the arc
    /// author into the root layer, so a single journaled wrapper captures the
    /// whole operation as one undo unit.
    #[cfg(feature = "authoring")]
    pub fn add_asset_child_arc(
        &mut self,
        parent_path: &str,
        asset_path: &str,
        as_reference: bool,
        target_prim: Option<&str>,
    ) -> Result<Option<String>, SessionError> {
        if self.service.prim_at_path(parent_path).is_none() {
            return Ok(None);
        }
        let base = prim_name_from_asset(asset_path);
        let parent = parent_path.trim_end_matches('/');
        // Find the lowest free "<base>", "<base>1", "<base>2", … under parent.
        let mut candidate = format!("{parent}/{base}");
        let mut suffix = 1u32;
        while self.service.prim_at_path(&candidate).is_some() {
            candidate = format!("{parent}/{base}{suffix}");
            suffix += 1;
        }
        self.define_prim(&candidate, "Xform")?;
        if as_reference {
            self.add_reference(&candidate, asset_path, target_prim)?;
        } else {
            self.add_payload(&candidate, asset_path, target_prim)?;
        }
        Ok(Some(candidate))
    }

    /// Switch the stage's edit target to the layer identified by `identifier`
    /// (where subsequent authoring writes land). No composition change, but the
    /// Layer Stack panel's edit-target highlight must refresh, so we bump
    /// `topology_generation` (which the shell keys its layer-stack cache on).
    #[cfg(feature = "authoring")]
    pub fn set_edit_target(&mut self, identifier: &str) -> Result<(), SessionError> {
        self.service
            .apply_layer_edit(&idtx_usd::LayerEdit::SetEditTarget {
                identifier: identifier.to_owned(),
            })?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        Ok(())
    }

    /// Mute the layer identified by `identifier`, dropping its contribution from
    /// composition. No-ops at the FFI level if the identifier does not resolve.
    #[cfg(feature = "authoring")]
    pub fn mute_layer(&mut self, identifier: &str) -> Result<(), SessionError> {
        self.service.apply_layer_edit(&idtx_usd::LayerEdit::Mute {
            identifier: identifier.to_owned(),
        })?;
        self.service.project_topology()?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        Ok(())
    }

    /// Un-mute the layer identified by `identifier`, restoring its contribution.
    #[cfg(feature = "authoring")]
    pub fn unmute_layer(&mut self, identifier: &str) -> Result<(), SessionError> {
        self.service
            .apply_layer_edit(&idtx_usd::LayerEdit::Unmute {
                identifier: identifier.to_owned(),
            })?;
        self.service.project_topology()?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        Ok(())
    }

    /// Reorder the sublayer `identifier` within its `parent_identifier`'s
    /// sublayer list (empty parent = the root layer). `stronger = true` moves it
    /// earlier (a stronger opinion), `false` later (weaker). Changes which
    /// opinion wins, so the topology is reprojected.
    #[cfg(feature = "authoring")]
    pub fn move_sublayer(
        &mut self,
        identifier: &str,
        parent_identifier: &str,
        stronger: bool,
    ) -> Result<(), SessionError> {
        self.service
            .apply_layer_edit(&idtx_usd::LayerEdit::MoveSublayer {
                identifier: identifier.to_owned(),
                parent_identifier: parent_identifier.to_owned(),
                stronger,
            })?;
        self.service.project_topology()?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        Ok(())
    }

    /// Capture the current root-layer opinion of `property` on the prim at
    /// `path` as a serialized `.usda` property spec, or `""` when the property
    /// is not authored in the root layer.
    ///
    /// The returned text is an opaque, lossless snapshot: pass it back to
    /// [`Self::restore_property_opinion`] to re-author the exact opinion (used
    /// by the undo direction of a "remove opinion" edit). Silently yields `""`
    /// when `path` is not present in the current stage.
    #[cfg(feature = "authoring")]
    pub fn snapshot_root_property(
        &self,
        path: &str,
        property: &str,
    ) -> Result<String, SessionError> {
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(String::new());
        };
        Ok(self.service.root_property_text(prim_id, property)?)
    }

    /// Remove the root-layer opinion of `property` on the prim at `path`.
    ///
    /// Removes only the root edit-target spec (via `RemoveProperty`), so the
    /// composed value reverts to whatever weaker layers / schema fallback
    /// provide. The prim's own root-layer spec (`def`/`over`) is untouched, so
    /// `is_locally_authored` — and thus editability — is unchanged; only the one
    /// property opinion disappears. The stage edit fires an `ObjectsChanged`
    /// notice drained by the next [`tick`][Self::tick], which reprojects the
    /// viewport and bumps `change_generation` (refreshing the inspector's cached
    /// `inspect_prim`, so the opinion stack / `root_authored` flag update).
    ///
    /// Silently no-ops when `path` is not present in the current stage.
    #[cfg(feature = "authoring")]
    pub fn clear_property_opinion(
        &mut self,
        path: &str,
        property: &str,
    ) -> Result<(), SessionError> {
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(());
        };
        self.service
            .replace_root_property_text(prim_id, property, "")?;
        #[cfg(feature = "remote")]
        if !self.collab_applying {
            // The server protocol has no clear/default-block command.
            self.preserve_attribute_clear(path, property);
            if let Some(state) = &mut self.collab_persistence { state.local_only_dirty = true; }
        }
        Ok(())
    }

    /// Re-author a root-layer property opinion from a snapshot previously
    /// captured by [`Self::snapshot_root_property`] — the undo direction of
    /// [`Self::clear_property_opinion`].
    ///
    /// Silently no-ops when `path` is not present in the current stage or the
    /// snapshot is empty (nothing was authored in the root layer).
    #[cfg(feature = "authoring")]
    pub fn restore_property_opinion(
        &mut self,
        path: &str,
        property: &str,
        snapshot: &str,
    ) -> Result<(), SessionError> {
        if snapshot.is_empty() {
            return Ok(());
        }
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(());
        };
        #[cfg(feature = "remote")]
        {
            self.observe_collab_root(false)?;
            self.capture_attribute_authority(path, property)?;
        }
        self.service
            .replace_root_property_text(prim_id, property, snapshot)?;
        #[cfg(feature = "remote")]
        self.publish_attributes(path, &[property.to_owned()])?;
        Ok(())
    }

    /// Apply a single-apply API schema (e.g. `"ShadowAPI"`) to the prim at
    /// `path`, authored into the root layer.
    ///
    /// Used by the light inspector's **Add Shadow Casting** button: applying
    /// `UsdLuxShadowAPI` is what the renderer treats as "this light casts"
    /// (`HasAPI<ShadowAPI>`), so no extra `shadow:enable` write is needed.
    ///
    /// Applying an API schema changes the prim's composed metadata
    /// (`apiSchemas`) and adds its attributes, so — like [`Self::create_opinion`]
    /// — the affected path is reprojected and `topology_generation` advanced to
    /// force the shell to rebuild its cached views and the renderer to re-extract
    /// (the light now casts). Silently no-ops when `path` is absent.
    #[cfg(feature = "authoring")]
    pub fn apply_api_schema(&mut self, path: &str, schema: &str) -> Result<(), SessionError> {
        if self.service.prim_at_path(path).is_none() {
            return Ok(());
        }
        self.service
            .apply_structural_command(&idtx_usd::StructuralCommand::ApplyApi {
                path: path.to_owned(),
                schema: schema.to_owned(),
            })?;
        self.service.project_topology_paths(&[path.to_owned()])?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        self.snapshot_dirty = true;
        Ok(())
    }

    /// Make the gprim at `path` collidable with the `OpenUSD`-standard schemas
    /// (collision inspector's **Add Collision**): apply `UsdPhysicsCollisionAPI`
    /// (+ `UsdPhysicsMeshCollisionAPI` with `physics:approximation = none` when
    /// the prim is a Mesh) and the reduced `IdtxCollisionAPI` addendum (so the
    /// inspector's channel checkboxes and later hull generation have a schema to
    /// author into). `physics:collisionEnabled` is authored `true`.
    ///
    /// The schemas are applied as **one failure-atomic batch** guarded by an
    /// observer checkpoint: any mid-sequence failure rolls every opinion back, so
    /// a collider is never left half-applied. Returns the exact ordered list of
    /// schemas applied, so the shell can journal an undo that removes precisely
    /// that set (see `EditCommand::ApplyColliderSchemas`).
    ///
    /// Like [`Self::apply_api_schema`], the affected path is reprojected and
    /// `topology_generation` advanced so the shell and renderer refresh; the
    /// collision hydration then runs on the next topology projection. The collider
    /// shape and analytic dimensions are **resolved at hydration** from the gprim
    /// type + `physics:approximation` (no shape token / dimension opinions are
    /// authored here — the gprim's own geometry sizes the collider). Returns an
    /// empty list when `path` is absent.
    ///
    /// # Errors
    /// Propagates [`SessionError`] from the underlying authoring writes or the
    /// reprojection (after rolling the batch back).
    #[cfg(feature = "authoring")]
    pub fn apply_collision_api(&mut self, path: &str) -> Result<Vec<String>, SessionError> {
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(Vec::new());
        };
        // The prim's authored type decides whether the mesh-collision schema is
        // added (it applies only to a UsdGeomMesh).
        let is_mesh = self.prim_type_name(path) == "Mesh";

        let checkpoint = self.service.observer_checkpoint()?;
        let applied = match self.apply_collider_schemas(path, prim_id, is_mesh) {
            Ok(applied) => applied,
            Err(e) => {
                // Best-effort rollback of the partially-applied batch; surface the
                // original error. The underlying authoring fns already unlock the
                // shim lifecycle lock before any throw.
                let _ = self.service.observer_rollback_to(checkpoint);
                return Err(e);
            }
        };

        self.service.project_topology_paths(&[path.to_owned()])?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        self.snapshot_dirty = true;
        Ok(applied)
    }

    /// The write batch behind [`Self::apply_collision_api`], factored out so the
    /// caller can roll it back atomically on any error. Does **not** reproject —
    /// the caller does that once after the batch commits.
    #[cfg(feature = "authoring")]
    fn apply_collider_schemas(
        &mut self,
        path: &str,
        prim_id: idtx_usd::PrimId,
        is_mesh: bool,
    ) -> Result<Vec<String>, SessionError> {
        use idtx_usd::{PropertyValue, StructuralCommand};
        use idtx_usd_extension::{
            Approximation, ATTR_PHYSICS_APPROXIMATION, ATTR_PHYSICS_COLLISION_ENABLED,
            COLLISION_SCHEMA, PHYSICS_COLLISION_SCHEMA, PHYSICS_MESH_COLLISION_SCHEMA,
        };

        self.service
            .apply_structural_command(&StructuralCommand::ApplyApi {
                path: path.to_owned(),
                schema: PHYSICS_COLLISION_SCHEMA.to_owned(),
            })?;
        self.service
            .apply_structural_command(&StructuralCommand::SetProperty {
                prim: prim_id,
                property: ATTR_PHYSICS_COLLISION_ENABLED.to_owned(),
                value: PropertyValue::Bool(true),
            })?;
        let mut applied = vec![PHYSICS_COLLISION_SCHEMA.to_owned()];

        if is_mesh {
            self.service
                .apply_structural_command(&StructuralCommand::ApplyApi {
                    path: path.to_owned(),
                    schema: PHYSICS_MESH_COLLISION_SCHEMA.to_owned(),
                })?;
            // A fresh mesh collider defaults to colliding against the mesh itself.
            self.service
                .apply_structural_command(&StructuralCommand::SetProperty {
                    prim: prim_id,
                    property: ATTR_PHYSICS_APPROXIMATION.to_owned(),
                    value: PropertyValue::Token(Approximation::None.as_token().to_owned()),
                })?;
            applied.push(PHYSICS_MESH_COLLISION_SCHEMA.to_owned());
        }

        // Reduced IDTX addendum (channel defaults to All) so the collider is
        // dual-authored and the inspector channel/hull surface has a schema to
        // write into. A standard-only DCC simply ignores it.
        self.service
            .apply_structural_command(&StructuralCommand::ApplyApi {
                path: path.to_owned(),
                schema: COLLISION_SCHEMA.to_owned(),
            })?;
        applied.push(COLLISION_SCHEMA.to_owned());
        Ok(applied)
    }

    /// Remove the `OpenUSD`-standard collider schemas (and the reduced
    /// `IdtxCollisionAPI` addendum) previously applied by
    /// [`Self::apply_collision_api`] from the gprim at `path`, and clear every
    /// root-layer property opinion those schemas own — the collision inspector's
    /// **Remove Collision** button, the symmetric counterpart of **Add
    /// Collision**.
    ///
    /// Removing an API schema only edits the `apiSchemas` list-op; USD leaves the
    /// schema's authored attribute opinions behind as orphans. So this also
    /// clears the two standard collider attrs (`physics:collisionEnabled`,
    /// `physics:approximation`) and every root-authored `idtx:collision:*`
    /// attribute — crucially the pre-baked `idtx:collision:hull:points` /
    /// `:triangles` arrays, whose stale bulk is the real size win once the
    /// collider is gone.
    ///
    /// Only schemas actually applied are removed, so no spurious `delete
    /// apiSchemas` opinion is authored for a schema that was never present; only
    /// root-authored opinions are cleared, so weaker-layer values are untouched.
    /// Like [`Self::apply_collision_api`] the batch is guarded by an observer
    /// checkpoint (any mid-sequence failure rolls the whole batch back), then the
    /// path is reprojected and `topology_generation` advanced so the shell and
    /// renderer refresh. Returns the ordered list of schemas removed (empty when
    /// `path` is absent or carried no collider to remove); undo is handled by the
    /// shell's root-layer snapshot (`journaled_structural`), which captures the
    /// schema removal and the cleared opinions together.
    ///
    /// # Errors
    /// Propagates [`SessionError`] from the underlying authoring writes, the
    /// prim inspection, or the reprojection (after rolling the batch back).
    #[cfg(feature = "authoring")]
    pub fn remove_collision_api(&mut self, path: &str) -> Result<Vec<String>, SessionError> {
        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(Vec::new());
        };
        let (applied, owned) = self.collision_schemas_and_props(prim_id)?;
        if applied.is_empty() && owned.is_empty() {
            return Ok(Vec::new());
        }

        let checkpoint = self.service.observer_checkpoint()?;
        if let Err(e) = self.remove_collider_schemas_and_props(path, prim_id, &applied, &owned) {
            // Best-effort rollback of the partially-removed batch; surface the
            // original error.
            let _ = self.service.observer_rollback_to(checkpoint);
            return Err(e);
        }

        self.service.project_topology_paths(&[path.to_owned()])?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        self.snapshot_dirty = true;
        Ok(applied)
    }

    /// Inspect the prim and derive, in one FFI pass, (a) which collider schemas
    /// authored by [`Self::apply_collision_api`] are currently applied and (b)
    /// the names of the collision-owned, root-authored property opinions to
    /// clear. See [`Self::remove_collision_api`].
    #[cfg(feature = "authoring")]
    fn collision_schemas_and_props(
        &mut self,
        prim_id: idtx_usd::PrimId,
    ) -> Result<(Vec<String>, Vec<String>), SessionError> {
        use idtx_usd_extension::{
            ATTR_PHYSICS_APPROXIMATION, ATTR_PHYSICS_COLLISION_ENABLED, COLLISION_NS,
            COLLISION_SCHEMA, PHYSICS_COLLISION_SCHEMA, PHYSICS_MESH_COLLISION_SCHEMA,
        };

        let records = self.service.inspect_prim(prim_id)?;

        // The collider schemas Add Collision / hull generation apply, filtered to
        // those actually listed in `apiSchemas` (so a schema never applied is not
        // "removed" into a spurious `delete apiSchemas` opinion).
        let summary = records
            .iter()
            .find(|r| r.name == "apiSchemas")
            .map_or("", |r| r.value_summary.as_str());
        let present: std::collections::HashSet<&str> =
            summary.split(", ").map(str::trim).collect();
        let applied: Vec<String> = [
            PHYSICS_COLLISION_SCHEMA,
            PHYSICS_MESH_COLLISION_SCHEMA,
            COLLISION_SCHEMA,
        ]
        .into_iter()
        .filter(|s| present.contains(*s))
        .map(str::to_owned)
        .collect();

        // Root-authored property opinions the collision schemas own: the two
        // standard collider attrs + every `idtx:collision:*` attr (channel + the
        // pre-baked hull point/triangle arrays).
        let collision_prefix = format!("{COLLISION_NS}:");
        let owned: Vec<String> = records
            .iter()
            .filter(|r| r.root_authored)
            .filter(|r| {
                r.name == ATTR_PHYSICS_COLLISION_ENABLED
                    || r.name == ATTR_PHYSICS_APPROXIMATION
                    || r.name.starts_with(&collision_prefix)
            })
            .map(|r| r.name.clone())
            .collect();

        Ok((applied, owned))
    }

    /// The write batch behind [`Self::remove_collision_api`]: remove each applied
    /// collider `schema` from the `apiSchemas` list-op, then clear each owned
    /// root-layer property opinion. Factored out so the caller can roll it back
    /// atomically on any error; does **not** reproject.
    #[cfg(feature = "authoring")]
    fn remove_collider_schemas_and_props(
        &mut self,
        path: &str,
        prim_id: idtx_usd::PrimId,
        schemas: &[String],
        properties: &[String],
    ) -> Result<(), SessionError> {
        use idtx_usd::StructuralCommand;

        for schema in schemas {
            self.service
                .apply_structural_command(&StructuralCommand::RemoveApi {
                    path: path.to_owned(),
                    schema: schema.clone(),
                })?;
        }
        // Removing the schema leaves its authored attribute specs behind, so clear
        // the root edit-target opinion of each (mirrors `clear_property_opinion`).
        for property in properties {
            self.service
                .replace_root_property_text(prim_id, property, "")?;
        }
        Ok(())
    }

    /// Gather the mesh point cloud for a collision-hull generation on the prim
    /// at `path`, expressed in **that prim's local space** (the space
    /// `idtx:collision:hull:points` is authored in and the wireframe renders in).
    ///
    /// Honors `OpenUSD`'s "do not nest gprims under gprims" rule — gprims are leaf
    /// geometry, so the gather prunes at every gprim:
    /// - If the collision prim is itself a gprim (a Mesh), only its own
    ///   `MeshComp` points are used; no children are visited.
    /// - Otherwise (an Xform/Scope/…) the subtree is walked, descending through
    ///   arbitrarily nested non-gprims and collecting the `MeshComp` of every
    ///   Mesh reached — but never recursing *past* a gprim (a gprim with a gprim
    ///   ancestor inside the subtree is skipped).
    ///
    /// Non-Mesh gprims contribute no points but still prune their descendants.
    /// Returns an empty cloud when `path` is unknown or carries no mesh geometry.
    #[cfg(feature = "authoring")]
    #[must_use]
    pub fn gather_collision_mesh_points(&self, path: &str) -> Vec<[f32; 3]> {
        let world = self.hydrator.world();
        let Some(root_entity) = self.hydrator.entity_for_path(path) else {
            return Vec::new();
        };
        // The collision prim's world transform; its inverse maps every gathered
        // mesh's world-space points back into the collision prim's local space.
        let root_world = world
            .get::<idtx_scene::WorldTransform>(root_entity)
            .map_or(Mat4::IDENTITY, |w| w.matrix);
        let to_local = root_world.inverse();

        // The leaf gprims under `path` (the prim itself when it is a gprim) — the
        // same traversal the behavior highlight resolve uses. Each contributing
        // Mesh's points are transformed into the collision prim's local space;
        // non-Mesh gprims carry no `MeshComp` and so contribute nothing.
        let mut out = Vec::new();
        for entity in leaf_gprim_entities(&self.hydrator, path) {
            let Some(mesh) = world.get::<idtx_scene::MeshComp>(entity) else {
                continue;
            };
            let mesh_world = world
                .get::<idtx_scene::WorldTransform>(entity)
                .map_or(Mat4::IDENTITY, |w| w.matrix);
            let m = to_local * mesh_world;
            out.extend(
                mesh.points
                    .iter()
                    .map(|p| m.transform_point3(Vec3::from(*p)).to_array()),
            );
        }
        out
    }

    /// Author a generated convex hull into the collision prim at `path`:
    /// `idtx:collision:hull:points` (`point3f[]`) and
    /// `idtx:collision:hull:triangles` (`int[]`) on the reduced `IdtxCollisionAPI`
    /// addendum, and stamp the `OpenUSD`-standard `physics:approximation = convexHull`
    /// (ensuring `UsdPhysicsMeshCollisionAPI` + the reduced addendum are applied)
    /// so a standard DCC reads a convex-hull mesh collider and IDTX consumes the
    /// pre-baked hull verbatim (never regenerated at runtime). Mirrors
    /// [`Self::apply_collision_api`]'s write-then-reproject shape so the ECS and
    /// renderer refresh. Silently no-ops when `path` is absent.
    ///
    /// Undo is handled by the shell's root-layer snapshot (`journaled_structural`),
    /// which captures the schema applications and the hull/approximation opinions
    /// together — no bespoke undo command is needed.
    ///
    /// # Errors
    /// Propagates [`SessionError`] from the underlying authoring writes or the
    /// reprojection.
    #[cfg(feature = "authoring")]
    pub fn author_collision_hull(
        &mut self,
        path: &str,
        points: &[[f32; 3]],
        triangles: &[i32],
    ) -> Result<(), SessionError> {
        use idtx_usd::{PropertyValue, StructuralCommand};
        use idtx_usd_extension::{
            Approximation, ATTR_HULL_POINTS, ATTR_HULL_TRIANGLES, ATTR_PHYSICS_APPROXIMATION,
            COLLISION_SCHEMA, PHYSICS_MESH_COLLISION_SCHEMA,
        };

        let Some(prim_id) = self.service.prim_at_path(path) else {
            return Ok(());
        };
        // Ensure both schemas that own the authored attributes are applied
        // (idempotent): PhysicsMeshCollisionAPI carries physics:approximation, the
        // reduced IdtxCollisionAPI carries the hull arrays.
        for schema in [PHYSICS_MESH_COLLISION_SCHEMA, COLLISION_SCHEMA] {
            self.service
                .apply_structural_command(&StructuralCommand::ApplyApi {
                    path: path.to_owned(),
                    schema: schema.to_owned(),
                })?;
        }
        self.service
            .apply_structural_command(&StructuralCommand::SetProperty {
                prim: prim_id,
                property: ATTR_PHYSICS_APPROXIMATION.to_owned(),
                value: PropertyValue::Token(Approximation::ConvexHull.as_token().to_owned()),
            })?;
        self.service
            .apply_structural_command(&StructuralCommand::SetProperty {
                prim: prim_id,
                property: ATTR_HULL_POINTS.to_owned(),
                value: PropertyValue::Point3Array(points.to_vec()),
            })?;
        self.service
            .apply_structural_command(&StructuralCommand::SetProperty {
                prim: prim_id,
                property: ATTR_HULL_TRIANGLES.to_owned(),
                value: PropertyValue::IntArray(triangles.to_vec()),
            })?;
        self.service.project_topology_paths(&[path.to_owned()])?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        self.snapshot_dirty = true;
        Ok(())
    }

    /// The hydrated prim's authored USD type name (`"Sphere"`, `"Mesh"`, …), or
    /// an empty string when the path has no hydrated entity / `PrimNode`.
    #[cfg(feature = "authoring")]
    fn prim_type_name(&self, path: &str) -> String {
        self.hydrator
            .entity_for_path(path)
            .and_then(|e| self.hydrator.world().get::<idtx_scene::PrimNode>(e))
            .map(|node| node.type_name.clone())
            .unwrap_or_default()
    }

    /// Remove a single-apply API schema previously applied by
    /// [`Self::apply_api_schema`] — the symmetric undo of **Add Shadow Casting**.
    #[cfg(feature = "authoring")]
    pub fn remove_api_schema(&mut self, path: &str, schema: &str) -> Result<(), SessionError> {
        if self.service.prim_at_path(path).is_none() {
            return Ok(());
        }
        self.service
            .apply_structural_command(&idtx_usd::StructuralCommand::RemoveApi {
                path: path.to_owned(),
                schema: schema.to_owned(),
            })?;
        self.service.project_topology_paths(&[path.to_owned()])?;
        self.topology_generation = self.topology_generation.wrapping_add(1);
        self.snapshot_dirty = true;
        Ok(())
    }

    /// `true` when the stage's root layer holds unsaved authoring edits.
    ///
    /// Set by [`Self::tick`] when a change batch touched the root layer while
    /// authoring; cleared by [`Self::mark_stage_saved`]. Session-layer edits
    /// (visibility overrides, variant selection, runtime write-back) never set
    /// it, so this reflects only changes that an in-place save would persist.
    #[cfg(feature = "authoring")]
    #[must_use]
    pub fn is_stage_dirty(&self) -> bool {
        #[cfg(feature = "remote")]
        if self.collab.as_ref().is_some_and(|client| client.has_uncommitted_intent())
            || self.collab_persistence.as_ref().is_some_and(|state| state.local_only_dirty
                || state.committing.is_some() || self.service.root_layer_content_hash().ok() != state.observed_root) {
            return true;
        }
        self.stage_dirty
    }

    /// Clear the unsaved-changes flag after the stage has been persisted to
    /// disk (in-place save, Save As export, or usdz repack) or reloaded fresh.
    #[cfg(feature = "authoring")]
    pub fn mark_stage_saved(&mut self) {
        self.stage_dirty = false;
        #[cfg(feature = "remote")]
        let transforms = self.root_transform_opinions().ok();
        #[cfg(feature = "remote")]
        if let Some(state) = &mut self.collab_persistence {
            state.local_only_dirty = false;
            state.observed_root = self.service.root_layer_content_hash().ok();
            state.non_transform = self.service.root_non_sync_text().ok();
            if let Some(transforms) = transforms { state.transform_opinions = transforms; }
        }
        // Snapshot the just-persisted root-layer content. `SdfLayer::IsDirty`
        // stays `true` after an `Export`-based save, so the tick probe below
        // compares the live root against this baseline rather than trusting the
        // never-cleared bit — otherwise the next change batch (even a
        // session-layer edit) would immediately re-set the dirty marker.
        self.saved_root_hash = self.service.root_layer_content_hash().ok();
    }

    /// Save the root layer of the open stage to its current on-disk location.
    ///
    /// This is an in-place save — it does not flatten the stage. Only the root
    /// layer is written; sublayer files are left untouched.
    #[cfg(feature = "authoring")]
    pub fn save_stage(&self, destination: impl AsRef<std::path::Path>) -> Result<(), SessionError> {
        self.service
            .save_stage(destination)
            .map(|_| ())
            .map_err(SessionError::from)
    }

    /// Export the root layer of the open stage to a new file path.
    ///
    /// The destination extension determines the output format (usda / usdc).
    /// For usdz destinations use [`save_as_usdz`] instead.
    /// This never flattens the stage — only the root layer is exported.
    #[cfg(feature = "authoring")]
    pub fn export_root_layer(
        &self,
        destination: impl AsRef<std::path::Path>,
    ) -> Result<(), SessionError> {
        self.service
            .export_root_layer(destination)
            .map(|_| ())
            .map_err(SessionError::from)
    }

    /// Export a fully **flattened** composition of the open stage to a new file.
    ///
    /// Unlike [`export_root_layer`], this bakes every composition arc — including
    /// the *session-layer* opinions that carry unsaved authoring edits (variant
    /// selections, transform overrides) — into a single layer. Used to hand the
    /// current on-screen state to an out-of-process runtime preview: writing the
    /// flattened stage next to the original source keeps relative asset
    /// references (textures, referenced sublayers) resolvable.
    #[cfg(feature = "authoring")]
    pub fn export_flattened_stage(
        &self,
        destination: impl AsRef<std::path::Path>,
    ) -> Result<(), SessionError> {
        self.service
            .export_flattened_stage(destination)
            .map(|_| ())
            .map_err(SessionError::from)
    }

    /// Save or export the stage as a `.usdz` package.
    ///
    /// Two cases:
    /// - **Source is usdz**: the existing package members are preserved and
    ///   only the internal root layer is rewritten.  Delegates to `save_stage`
    ///   which handles the full unpack → re-export → repack → verify flow.
    /// - **Source is loose (usda / usdc)**: the root layer is exported to a
    ///   temporary sibling file next to the source, then
    ///   `UsdUtilsCreateNewUsdzPackage` collects all dependencies and creates a
    ///   self-contained `.usdz` at `destination`.  Exporting next to the source
    ///   (rather than into an unrelated temp dir) keeps relative asset
    ///   references resolvable.  The temporary file is removed afterwards.
    #[cfg(feature = "authoring")]
    pub fn save_as_usdz(
        &self,
        destination: impl AsRef<std::path::Path>,
    ) -> Result<(), SessionError> {
        let destination = destination.as_ref();
        let src_ext = self
            .source_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        if src_ext == "usdz" {
            // Source is already packaged — delegate to save_stage which handles
            // the usdz→usdz extract-rewrite-repack-verify cycle in C++.
            return self
                .service
                .save_stage(destination)
                .map(|_| ())
                .map_err(SessionError::from);
        }

        // Loose source → usdz: export the root layer to a temporary sibling of
        // the source (so relative asset references still resolve), then let
        // UsdUtilsCreateNewUsdzPackage collect all dependencies and create the
        // package.  Errors (including unresolvable assets) are surfaced instead
        // of silently reporting success.
        self.service
            .package_loose_as_usdz(&self.source_path, destination)
            .map(|_| ())
            .map_err(SessionError::from)
    }

    /// Returns `(view_proj, eye_world)` for the camera the viewport is currently
    /// rendered through — the authored scene camera when one is active, else the
    /// orbit camera. Used by the shell to unproject viewport click coordinates
    /// into a world-space ray.
    ///
    /// Resolves the camera through [`active_camera_desc`](Self::active_camera_desc),
    /// the same source [`render_one_frame`](Self::render_one_frame) draws with, so
    /// picking stays locked to whatever camera is on screen (scene camera on open,
    /// or the orbit camera after any navigation / orthographic preset). Returning
    /// a stale orbit camera here while the frame rendered through a scene camera is
    /// what previously sent every pick ray into empty space.
    pub fn camera_view_proj(&mut self) -> (Mat4, Vec3) {
        let aspect = self.viewport_pixels[0] as f32 / self.viewport_pixels[1] as f32;
        let desc = self.active_camera_desc(aspect);
        let view_proj = desc.projection * desc.view;
        // Eye = inverse-view translation, so it matches whichever camera is active
        // (the caller currently ignores it; kept correct for future use).
        let eye = desc.view.inverse().col(3).truncate();
        (view_proj, eye)
    }

    /// Casts a world-space ray through the scene and resolves the pick target
    /// respecting packed-stage expansion state.
    ///
    /// Returns the USD path of the resolved selection, or `None` on miss.
    /// `packed_revealed` is the set of path hashes currently expanded in the scene tree.
    pub fn pick_and_resolve(
        &mut self,
        ray_origin: Vec3,
        ray_dir: Vec3,
        packed_revealed: &HashSet<u64>,
    ) -> Option<String> {
        // Pick rays arrive in render world space.  The ECS WorldTransform and
        // BoundsComp components are in USD world space.  Transform the ray back
        // to USD space using the inverse of the projection basis.
        let usd_origin = self.projection_basis_inv.transform_point3(ray_origin);
        let usd_dir = self
            .projection_basis_inv
            .transform_vector3(ray_dir)
            .normalize_or_zero();
        let hit = idtx_scene::picking::pick_prim(self.hydrator.world_mut(), usd_origin, usd_dir)?;
        resolve_pick_path(&self.hydrator, hit.entity, packed_revealed)
    }

    // ── USD change tick ───────────────────────────────────────────────────────

    /// Returns `true` when a change's domain bitmask represents a pure rigid
    /// transform edit that the transform-patch fast path can service without a
    /// whole-stage re-extract.
    ///
    /// An `xformOp*` property write classifies as `TRANSFORM | INSPECTION` (the
    /// `INSPECTION` bit rides along on every property write — see
    /// `usd_shim.cpp` `classify`), so an exact `== TRANSFORM` comparison never
    /// matches a real controller pose write and the fast path was silently dead.
    /// A change qualifies when `TRANSFORM` is set and no bits outside
    /// `{TRANSFORM, INSPECTION}` are present — anything else (`GEOMETRY`,
    /// `MATERIAL`, `SKELETON`, prim-level `METADATA`, …) still needs a full
    /// re-extract.
    fn is_transform_only_change(domains: DirtyDomains) -> bool {
        const ALLOWED: u32 = DirtyDomains::TRANSFORM.bits() | DirtyDomains::INSPECTION.bits();
        domains.contains(DirtyDomains::TRANSFORM) && (domains.bits() & !ALLOWED) == 0
    }

    /// Whether a change record is a **render-irrelevant physics-authority write**:
    /// a value-only (non-resync) edit to a `physics:*` attribute — e.g. the Phase
    /// 2A `physics:startsAsleep` sleep-sync the runtime physics tick authors back
    /// on every sleep/wake edge. These carry no geometry / transform / material
    /// change, yet the bridge classifies an unknown `physics:*` property as
    /// `GENERIC`, which is outside the `TRANSFORM | INSPECTION` fast-patch set and
    /// would force a full whole-stage re-extract — re-marshaling the dense
    /// `UsdCookie` mesh (~118k pts) across FFI on every edge and defeating the
    /// transform-only patch path (visible as hitching / sim slow-motion while a
    /// scene is actively grabbed). The reprojection router excludes them from the
    /// routing decision and the root set so a frame that only moved bodies and
    /// flipped sleep flags still takes the cheap transform patch. A `physics:*`
    /// *resync* (first-time spec authoring / topology) is NOT excluded — it still
    /// re-hydrates.
    fn is_physics_authority_change(change: &idtx_usd::ChangedPrim) -> bool {
        !change.resync
            && change
                .property_name
                .as_deref()
                .is_some_and(|p| p.starts_with("physics:"))
    }

    /// Whether a change record is a **render-irrelevant `OpenExec` plumbing write**:
    /// a value-only (non-resync) edit to an `outputs:*` attribute. The `OpenExec`
    /// tick authors each datasource's raw JSON payload onto its `outputs:data`
    /// (see `ApplyPendingUpdates` in `usd_exec_bridge.cpp`) on every interval that
    /// delivers data, plus any intermediate compute-node output. None of these is
    /// rendered — the rendered result of a compute chain is authored separately
    /// onto the *consumer's* `idtx:widget:*` / `xformOp:*` / material attributes —
    /// yet the bridge classifies an `outputs:*` property as `GENERIC`, outside the
    /// `TRANSFORM | INSPECTION` fast-patch set, so without this filter a datasource
    /// response whose downstream computed value happened to stay equal (change-
    /// skipped in the C++ bridge) would still force a full whole-stage re-extract
    /// every interval (the visible stutter). The reprojection router excludes these
    /// from the routing decision and the root set exactly like
    /// [`Self::is_physics_authority_change`]. An `outputs:*` *resync* (first-time
    /// spec authoring / topology) is NOT excluded — it still re-hydrates.
    fn is_exec_internal_change(change: &idtx_usd::ChangedPrim) -> bool {
        !change.resync
            && change
                .property_name
                .as_deref()
                .is_some_and(|p| p.starts_with("outputs:"))
    }

    /// Merge `new` deferred transform patches into the pending set instead of
    /// replacing it.
    ///
    /// Several reprojection passes run per rendered frame — the start-of-frame
    /// drain, each behavior's `apply_session_edits` (e.g. the camera-followed
    /// player capsule, a grabbed prim), and the physics write-back — but the
    /// patches are applied to the warm snapshot only once, at render (via
    /// `snapshot_transform_patches.take()`). A plain assignment let a later pass
    /// silently DROP the patches an earlier pass authored the same frame. The
    /// physics write-back runs last AND only while some rigid body is awake, so a
    /// restless body (e.g. a cookie jittering on the floor) made physics reproject
    /// every frame and clobber the behavior-authored patches — a grabbed prim or
    /// the player capsule stopped tracking the camera whenever ANY body was awake,
    /// yet worked once the scene settled and the physics pass early-exited. This is
    /// how one non-resting body breaks unrelated behaviors.
    ///
    /// Merging by path (latest value wins — the same prim moved twice keeps the
    /// newer pose) preserves every pass's patches. `take` at render clears the set,
    /// so it never accumulates across frames.
    fn merge_transform_patches(&mut self, new: Vec<(String, [f32; 16])>) {
        merge_transform_patches_into(&mut self.snapshot_transform_patches, new);
    }

    /// Whether a changed root `path` is an ancestor, descendant, or equal of any
    /// skinned/skeleton prim path (plan Phase 2B). Moving such a root may require
    /// a joint-palette resample, so it must take the projection path rather than
    /// the cheap matrices-only sample; a plain rigid root (a physics body like the
    /// dense `UsdCookie`) that touches none of these is routed to the fast sample
    /// even while the shipped skinned XR controllers keep `has_skeletons()` true.
    fn path_touches_skinned(path: &str, skin_paths: &[String]) -> bool {
        skin_paths.iter().any(|s| {
            // equal, or `s` is a descendant of `path` (path is an ancestor), or
            // `path` is a descendant of `s` — compared on the `/`-delimited
            // namespace so `/World/Cookie` never matches `/World/CookieJar`.
            s == path
                || (s.len() > path.len()
                    && s.as_bytes()[path.len()] == b'/'
                    && s.starts_with(path))
                || (path.len() > s.len()
                    && path.as_bytes()[s.len()] == b'/'
                    && path.starts_with(s.as_str()))
        })
    }

    /// reprojections according to the domain bitmask on each changed prim.
    ///
    /// **Routing policy:**
    /// - Any prim with the `TOPOLOGY` bit set or a resync flag triggers
    ///   `project_topology_paths` (affected roots only) followed by
    ///   `project_viewport_roots` when every value domain is covered by those
    ///   roots; mixed/ambiguous batches use full projection and reconciliation.
    /// - Value-only changes call `project_viewport_roots` for only the changed
    ///   prim IDs; unaffected ECS components remain untouched.
    ///
    /// Returns `true` if any changes were processed, `false` if the stage was
    /// quiet this tick.
    pub fn tick(&mut self) -> Result<bool, SessionError> {
        let Some(batch) = self.service.drain_changes()? else {
            // No USD changes — still process any queued inbound remote edits.
            #[cfg(all(feature = "remote", feature = "authoring"))]
            self.apply_inbound_collab()?;
            return Ok(false);
        };
        #[cfg(all(feature = "remote", feature = "authoring"))]
        self.observe_collab_root(false)?;

        // ── Unsaved-changes (dirty) tracking ──────────────────────────────────
        // A change batch arrived; decide whether it dirtied the ROOT layer. Only
        // while authoring — at runtime the physics/behavior write-back targets
        // the session layer and must never mark the saved document dirty.
        //
        // The authoritative signal is the root layer's own `IsDirty` bit: session
        // -layer edits (variant selection, visibility overrides, runtime write-
        // back) leave it clear, so this never false-positives. (The per-change
        // `authored_in_root_layer` flag can't be used here — for a resync it
        // reports whether the root merely *has a spec* at the path, which is true
        // for a session-layer variant switch on a root-defined prim.) Short-
        // circuiting on `!stage_dirty` keeps the probe off the hot path once set.
        //
        // `IsDirty` is necessary but not sufficient: an `Export`-based save (both
        // in-place loose Save and Save As go through `export_root_layer`) does NOT
        // clear the bit, so after a save it stays `true` forever. Confirm a set bit
        // against the baseline captured by `mark_stage_saved` — only mark dirty if
        // the root layer's content actually differs from the last persisted state.
        #[cfg(feature = "authoring")]
        if !self.stage_dirty
            && self.play_state == PlayState::Authoring
            && self.service.root_layer_is_dirty().unwrap_or(false)
        {
            let changed_since_save = match self.saved_root_hash {
                // Saved this session: dirty only if the root content has since
                // diverged from the baseline (a failed hash read errs on dirty).
                Some(baseline) => self
                    .service
                    .root_layer_content_hash()
                    .map_or(true, |current| current != baseline),
                // Never saved: a set dirty bit means genuine unsaved edits.
                None => true,
            };
            if changed_since_save {
                self.stage_dirty = true;
            }
        }

        let has_topology = batch.changes.iter()
            .any(|change| change.resync || change.domains.contains(DirtyDomains::TOPOLOGY));

        let mut patched = false;
        // Subtree scope for a localized (non-root) topology change — a runtime
        // spawn or single-prim retopology. `None` for a broad / whole-stage change
        // or a mixed batch with values outside those roots, keeping the full path.
        // Threaded into the FFI-heavy hydrate passes here AND in the tick tail
        // (which re-runs several of them), and into the scoped snapshot splice.
        let mut topo_scope: Option<std::collections::HashSet<idtx_usd::PrimId>> = None;
        #[cfg(feature = "physics")]
        let physics_was_current =
            self.physics.is_some() && self.physics_build_gen == Some(self.topology_generation);
        #[cfg(feature = "physics")]
        let mut physics_topology_change = None;
        if has_topology {
            let mut invalidation = invalidation::BatchInvalidation::from_changes(&batch.changes);
            // Preserve notice provenance: new child resyncs stay exact, while a
            // genuine ancestor resync in the same drain is never narrowed away.
            let delta = self.service
                .project_topology_paths(&invalidation.structural_paths)?;
            invalidation.include_topology(&delta, &self.hydrator, &self.service);
            #[cfg(feature = "physics")]
            {
                physics_topology_change = Some(physics::TopologyChangeSummary::capture(
                    &self.hydrator,
                    &delta,
                ));
            }
            self.hydrator.apply_topology_delta(&delta, &self.service);

            // Scope only when it covers the entire batch, not just its topology.
            // Otherwise all viewport/value-domain reconciliation runs below.
            let localized = invalidation.can_scope();

            let projection = if localized {
                // Scope = the delta's upsert prims (newly-spawned / retopologised,
                // incl. changed-in-place so the reconcile passes still fire).
                let mut scope: std::collections::HashSet<idtx_usd::PrimId> =
                    delta.upserts.iter().map(|r| r.prim).collect();
                let mut root_ids: Vec<idtx_usd::PrimId> = scope.iter().copied().collect();
                let mut projection = self.service.project_viewport_roots(&root_ids)?;
                // Always extend the scope with any point-instancer prototype roots
                // living OUTSIDE the spawned subtree, then re-project — so an
                // instancer in the subtree whose `prototypes` are external still
                // renders (the shim only emits prototype geometry under a requested
                // root). Native (`/__Prototype_N`) instances need no extension: the
                // shim re-emits them in full on a spawn resync.
                let extra: Vec<idtx_usd::PrimId> = projection
                    .point_instancers
                    .iter()
                    .flat_map(|pi| pi.prototypes.iter().copied())
                    .filter(|proto| !scope.contains(proto))
                    .collect();
                if !extra.is_empty() {
                    for p in extra {
                        if scope.insert(p) {
                            root_ids.push(p);
                        }
                    }
                    projection = self.service.project_viewport_roots(&root_ids)?;
                }
                // Append (not replace): preserve whole-stage diagnostics, matching
                // the value-only fast paths.
                self.diagnostics.extend(projection.diagnostics.clone());
                topo_scope = Some(scope);
                projection
            } else {
                let projection = self.service.project_viewport()?;
                self.diagnostics.clone_from(&projection.diagnostics);
                projection
            };
            self.hydrator.apply_viewport_projection(&projection)?;
            if self.snapshot_transform_patches.is_some() {
                // A topology projection can supersede an earlier pose in this
                // frame. Keep unrelated patches, but let these latest poses win.
                let patches = projection.transforms.iter()
                    .filter_map(|transform| {
                        self.service.path_for(transform.prim)
                            .map(|path| (path.to_owned(), transform.world_matrix))
                    })
                    .collect();
                self.merge_transform_patches(patches);
            }
            // Initialise VisibilityOverride for any entities that were just spawned.
            // (Self-gated + an O(N) effective-visibility recompute; left whole-stage.)
            self.hydrator.initialize_visibility_overrides(&self.service);
            // Hydrate BehaviorRef for any newly-spawned entities carrying an
            // authored `idtx:behavior:module` attachment. Existing refs are
            // preserved; freshly hydrated ones attach on the next
            // `attach_authored_behaviors` pass. (Self-gated; left whole-stage.)
            self.hydrator.hydrate_authored_behaviors(&self.service);
            // The FFI-heavy per-prim reconcile scans (PlayerRole / CollisionComp /
            // CollisionBodyComp / RigidBodyComp): scoped to the spawned subtree on a
            // localized change, whole-stage otherwise. Scoped to the upsert set
            // (not just brand-new prims) so reconcile-on-retopology is preserved.
            if let Some(scope) = &topo_scope {
                self.hydrator.hydrate_player_roles_scoped(&self.service, scope);
                self.hydrator.hydrate_collision_scoped(&self.service, scope);
                self.hydrator
                    .hydrate_collision_bodies_scoped(&self.service, scope);
                self.hydrator.hydrate_rigid_bodies_scoped(&self.service, scope);
            } else {
                self.hydrator.hydrate_player_roles(&self.service);
                self.hydrator.hydrate_collision(&self.service);
                self.hydrator.hydrate_collision_bodies(&self.service);
                self.hydrator.hydrate_rigid_bodies(&self.service);
            }
            // HudComp / WidgetPanelComp: ECS-type-gated (no FFI on non-HUD/panel
            // prims), left whole-stage.
            self.hydrator.hydrate_hud(&self.service);
            self.hydrator.hydrate_widgets(&self.service);

            // Refresh animation attributes and pseudo-root after topology change.
            // The single biggest unconditional FFI scan — scope it to the subtree.
            if let Some(scope) = &topo_scope {
                idtx_scene::animation::populate_anim_attributes_scoped(
                    self.hydrator.world_mut(),
                    &self.service,
                    scope,
                );
            } else {
                idtx_scene::animation::populate_all_anim_attributes(
                    self.hydrator.world_mut(),
                    &self.service,
                );
            }
            self.pseudo_root = self.hydrator.pseudo_root_entity();
            if let Some(root) = self.pseudo_root {
                idtx_scene::animation::attach_stage_anim_clip(
                    self.hydrator.world_mut(),
                    &self.service,
                    root,
                );
            }

            self.topology_generation = self.topology_generation.wrapping_add(1);
            // A re-hydration may have recreated prims or reset authored opinions,
            // so the visibility dedup cache can be stale — drop it and let the
            // next `set_visibility` re-author its edge.
            self.session_visibility_authored.clear();

            // Layer C: a purely-additive localized topology change takes the scoped
            // render-snapshot splice (insert only the new subtree, keep
            // `snapshot.serial` stable) instead of a full re-extract. Removals
            // (despawns) keep the full path. Mark `patched` so the `!patched` tail
            // does not force `snapshot_dirty`. `prepare_presentation` falls back to
            // a full extract if the splice meets skinning / instancing / a light,
            // or there is no warm snapshot yet.
            if let Some(scope) = &topo_scope
                && invalidation.can_splice()
            {
                invalidation::merge_topology_splice_into(&mut self.snapshot_topology_splice, scope);
                patched = true;
            }
        } else {
            // Render-relevant changes only: drop render-irrelevant physics-authority
            // writes (`physics:*` sleep-sync, see `is_physics_authority_change`) and
            // OpenExec plumbing writes (`outputs:*` datasource payloads, see
            // `is_exec_internal_change`) from the routing decision and the root set.
            // Without this a sleep/wake edge's `physics:startsAsleep` or a datasource
            // `outputs:data` payload (both classified `GENERIC`) forces a full
            // re-extract, re-marshaling the dense `UsdCookie` mesh every edge /
            // interval. For a scene with no physics or compute-graph writes this
            // filter is a no-op (predicates never match), so the common authoring
            // path is unchanged.
            let relevant: Vec<&idtx_usd::ChangedPrim> = batch
                .changes
                .iter()
                .filter(|c| {
                    !Self::is_physics_authority_change(c) && !Self::is_exec_internal_change(c)
                        && !c.property_name.as_deref().is_some_and(|name| name.starts_with("idtx:hud:"))
                })
                .collect();

            // Decide up front — from the change domains alone, before touching
            // the stage — whether every (render-relevant) change in this batch is a
            // pure transform edit (no resync; only the `TRANSFORM | INSPECTION` bits,
            // as an `xformOp` write classifies). If so we must NOT call
            // `project_viewport_roots`: that re-resolves materials, geometry and
            // lights for the whole moved subtree (re-hydration — and the source
            // of the `MaterialBindingAPI is not applied` warnings on a
            // default-prim move), when structurally nothing changed but placement.
            let transform_only = self.last_snapshot.is_some()
                && !relevant.is_empty()
                && relevant
                    .iter()
                    .all(|c| !c.resync && Self::is_transform_only_change(c.domains));

            // A collision-attribute value edit (dimensions, shape token, axis,
            // channel, hull arrays) is a value-only change that leaves the
            // stored `CollisionComp` stale: only the topology/resync path and
            // snapshot-restore re-hydrate it. Authoring a *new* collision
            // property spec resyncs (so a first-time dimension seed refreshes),
            // but re-authoring an already-authored dimension/shape is value-only
            // and never did — the editor wireframe (and picking narrow-phase)
            // kept showing the previous shape/size. Re-hydrate below when any
            // `idtx:collision:*` property changed.
            let collision_changed = batch.changes.iter().any(|c| {
                c.property_name
                    .as_deref()
                    .is_some_and(|p| p.starts_with("idtx:collision:"))
            });

            // Same value-only reconcile for widget panels: an `idtx:widget:*`
            // edit must re-stamp
            // WidgetPanelComp so the rasterizer re-draws on the next frame. Unlike
            // the shipped HUD path, widgets DO have a value-only trigger.
            let widget_changed = batch.changes.iter().any(|c| {
                c.property_name
                    .as_deref()
                    .is_some_and(|p| p.starts_with("idtx:widget:"))
            });

            // Value-only *geometry* edit: every render-relevant change is non-resync
            // and carries ONLY `GEOMETRY | INSPECTION` (a parametric size / radius /
            // height / axis / width / length writeback, e.g. an `OpenExec`-driven
            // cube `size`). Anything heavier (TRANSFORM / MATERIAL / LIGHT /
            // VISIBILITY / SKELETON / TOPOLOGY) clears it, so it never collides with
            // the transform-only fast path (TRANSFORM and GEOMETRY are disjoint
            // bits). This is domain-only; the actual fast path is gated at use on
            // every changed root being a patchable parametric gprim
            // (`is_patchable_gprim`) so a poly `MeshComp` falls through to the full
            // re-extract.
            const GEO_ALLOWED: u32 =
                DirtyDomains::GEOMETRY.bits() | DirtyDomains::INSPECTION.bits();
            let geometry_value_only = self.last_snapshot.is_some()
                && !relevant.is_empty()
                && relevant.iter().all(|c| {
                    !c.resync
                        && c.domains.contains(DirtyDomains::GEOMETRY)
                        && (c.domains.bits() & !GEO_ALLOWED) == 0
                });

            // A skinned subtree needs its joint palette refreshed when an
            // ancestor moves, and the transform-only sampler does not touch
            // poses. Only scenes that actually contain a skeleton (the shipped
            // skinned XR controller prefab) fall back to the projection path;
            // the common authoring scene has none and takes the fast path.
            if transform_only && !self.hydrator.has_skeletons() {
                // Transform-only fast path. `sample_transforms_only_default`
                // traverses each changed root and every active Xformable
                // descendant via `UsdGeomXformCache` — rigid subtrees, meshes and
                // lights alike — emitting only world matrices. No material /
                // geometry / bounds extraction, so no subtree re-hydration.
                let root_ids: Vec<_> = relevant.iter().map(|c| c.prim).collect();
                let transforms = self.service.sample_transforms_only_default(&root_ids)?;
                let patches = self.hydrator.apply_transform_samples(&transforms)?;
                if !patches.is_empty() {
                    // Patch the warm snapshot in place at render time (rewrites
                    // mesh and light world matrices; keeps `snapshot.serial`
                    // stable so the GPU buffer cache survives). MERGE, don't
                    // replace: an earlier pass this frame (a behavior's grab /
                    // capsule-follow write) may already have queued patches that a
                    // plain assignment would drop (see `merge_transform_patches`).
                    self.merge_transform_patches(patches);
                    patched = true;
                }
            } else if transform_only {
                // Phase 2B: transform-only edit while a skeleton exists elsewhere.
                // The shipped skinned XR controllers make `has_skeletons()`
                // globally true, which would otherwise force EVERY moving rigid
                // body onto the heavy `project_viewport_roots` path — re-marshaling
                // its full mesh across FFI every frame (the dense `UsdCookie` is
                // ~118k points). Split the changed roots: a plain rigid root takes
                // the cheap matrices-only sample; only roots that actually touch a
                // skinned subtree take the projection path (to resample joints).
                let skin_paths = self.hydrator.skinned_prim_paths();
                let mut plain_roots: Vec<_> = Vec::new();
                let mut skinned_roots: Vec<_> = Vec::new();
                for c in &relevant {
                    // Unknown path → treat as skinned (conservative: never skip a
                    // joint resample).
                    let touches_skin = self
                        .service
                        .path_for(c.prim)
                        .is_none_or(|p| Self::path_touches_skinned(p, &skin_paths));
                    if touches_skin {
                        skinned_roots.push(c.prim);
                    } else {
                        plain_roots.push(c.prim);
                    }
                }

                let mut merged: Vec<(String, [f32; 16])> = Vec::new();

                // Plain rigid roots: world matrices only — no geometry/material
                // re-marshal. This is what spares the cookie's dense mesh.
                if !plain_roots.is_empty() {
                    let transforms = self.service.sample_transforms_only_default(&plain_roots)?;
                    let samples = self.hydrator.apply_transform_samples(&transforms)?;
                    merged.extend(samples);
                }

                // Skinned-affecting roots: full projection so joints resample, then
                // the in-place Fix-3 patch when every bound skin is
                // `GpuClassicLinear`. If any is `CpuOpenUsd`/`RestPose` it cannot be
                // patched → force a full re-extract (drop the merged patches).
                let mut force_extract = false;
                if !skinned_roots.is_empty() {
                    let projection = self.service.project_viewport_roots(&skinned_roots)?;
                    self.diagnostics.extend(projection.diagnostics.clone());
                    self.hydrator.apply_viewport_projection(&projection)?;
                    let skinned_patchable = projection.skeleton_poses.is_empty()
                        || projection
                            .skin_bindings
                            .iter()
                            .all(|b| b.execution == idtx_usd::SkinningExecution::GpuClassicLinear);
                    if skinned_patchable {
                        for t in &projection.transforms {
                            if let Some(p) = self.service.path_for(t.prim) {
                                merged.push((p.to_owned(), t.world_matrix));
                            }
                        }
                        if !projection.skeleton_poses.is_empty() {
                            self.snapshot_skeleton_dirty = true;
                            patched = true;
                        }
                    } else {
                        force_extract = true;
                    }
                }

                if force_extract {
                    // Abandon the patches; the `!patched` tail forces a full
                    // re-extract (the ECS was still updated above, so it is correct).
                    self.snapshot_skeleton_dirty = false;
                } else if !merged.is_empty() {
                    // Merge (not replace) — see `merge_transform_patches`: another
                    // reprojection pass this frame may already hold patches.
                    self.merge_transform_patches(merged);
                    patched = true;
                }
            } else if relevant.is_empty() {
                // The batch carried ONLY render-irrelevant physics-authority writes
                // (e.g. a `physics:startsAsleep` flip with no paired pose): the warm
                // snapshot already reflects the scene, so nothing to reproject. Mark
                // handled so the `!patched` tail below does not force a needless full
                // re-extract (which would re-marshal the dense `UsdCookie` mesh).
                patched = true;
            } else if geometry_value_only
                && relevant
                    .iter()
                    .all(|c| self.hydrator.is_patchable_gprim(c.prim))
            {
                // Value-only geometry fast path. A parametric gprim's size / radius
                // / height / axis / width / length changed (e.g. an `OpenExec`
                // writeback onto a cube `size`) with nothing heavier in the batch,
                // and every changed root is a patchable gprim. Re-hydrate ONLY the
                // changed roots in the ECS (`project_viewport_roots` is scoped to
                // them), then patch their meshes into the warm snapshot at render
                // time via `patch_mesh_geometry`. The per-mesh `GpuMeshDesc::serial`
                // (content-hashed from the new params) moves, so the renderer
                // re-uploads just that prim and re-bakes just its merge group, while
                // `snapshot.serial` stays stable — every unrelated static-merge /
                // instance / material / AABB cache survives. Mirrors the transform /
                // skeleton / widget fast paths. A poly `MeshComp`, a prototype-only
                // prim, or any heavier domain fails the guard above and falls through
                // to the full reprojection below (correct: re-extracted there).
                let root_ids: Vec<_> = relevant.iter().map(|c| c.prim).collect();
                let projection = self.service.project_viewport_roots(&root_ids)?;
                self.diagnostics.extend(projection.diagnostics.clone());
                self.hydrator.apply_viewport_projection(&projection)?;
                self.snapshot_geometry_paths = relevant
                    .iter()
                    .filter_map(|c| self.service.path_for(c.prim).map(str::to_owned))
                    .collect();
                self.snapshot_geometry_dirty = true;
                patched = true;
            } else {
                // Full value-only reprojection: materials / geometry / lights /
                // visibility changed (not transform-only), so the whole moved
                // subtree must re-resolve and a full re-extract follows.
                let roots: Vec<_> = relevant.iter().map(|c| c.prim).collect();
                let projection = self.service.project_viewport_roots(&roots)?;
                self.diagnostics.extend(projection.diagnostics.clone());
                self.hydrator.apply_viewport_projection(&projection)?;
            }

            // Reconcile CollisionComp after a value-only collision edit so the
            // narrow-phase and the light-blue wireframe track the new shape and
            // dimensions (the topology branch already re-hydrates).
            if collision_changed {
                self.hydrator.hydrate_collision(&self.service);
            }
            if batch.changes.iter().any(|change| change.property_name.as_deref().is_some_and(|name| name.starts_with("idtx:hud:"))) {
                self.hydrator.hydrate_hud(&self.service);
            }
            if widget_changed {
                self.hydrator.hydrate_widgets(&self.service);
                // Value-only widget edit (`idtx:widget:*`, e.g. an OpenExec compute
                // writeback into a panel's text/value): if nothing in this batch
                // touched a heavier render domain (geometry/material/light/
                // visibility), patch just the affected panel(s) into the warm
                // snapshot at render time instead of forcing a whole-stage
                // re-extract — the per-interval stutter fix. Mirrors the
                // transform-only / skeleton-pose fast paths. A batch that also
                // carries a heavier domain falls through to the `!patched` full
                // re-extract below (correct — the panel is re-extracted there too).
                // New/removed widget prims arrive as a resync via the topology
                // branch above, so the value-only patch set is always stable.
                //
                // TRANSFORM and SKELETON count as heavy here too: a mixed batch
                // that also moves a prim (or re-poses a skinned subtree) is routed
                // to the full value-only reprojection (`else` arm above), which
                // updates the ECS but queues NO matrix/joint patch into the warm
                // snapshot. Letting the widget fast path mark that batch `patched`
                // would suppress the `!patched` full re-extract and leave the moved
                // transforms frozen on screen. An `idtx:widget:*` change itself
                // classifies as `INSPECTION | GENERIC` (never TRANSFORM/SKELETON),
                // so the common pure-widget writeback still takes the fast path.
                const HEAVY: u32 = DirtyDomains::TRANSFORM.bits()
                    | DirtyDomains::GEOMETRY.bits()
                    | DirtyDomains::MATERIAL.bits()
                    | DirtyDomains::LIGHT.bits()
                    | DirtyDomains::VISIBILITY.bits()
                    | DirtyDomains::SKELETON.bits();
                let any_heavy = batch
                    .changes
                    .iter()
                    .any(|c| (c.domains.bits() & HEAVY) != 0);
                if !any_heavy {
                    self.snapshot_widget_dirty = true;
                    patched = true;
                }
            }
        }

        // Visibility invalidation belongs to the batch, including mixed topology
        // + value drains, rather than only the value-only routing arm.
        if batch.changes.iter().any(|c| c.domains.contains(DirtyDomains::VISIBILITY)) {
            self.visibility_generation = self.visibility_generation.wrapping_add(1);
        }

        if !patched {
            // No fast path taken → force a full whole-stage re-extract next
            // render and drop any stale patches. A patched frame instead keeps
            // the warm snapshot and only rewrites matrices at render time.
            self.snapshot_dirty = true;
            self.snapshot_transform_patches = None;
            self.snapshot_skeleton_dirty = false;
            self.snapshot_geometry_dirty = false;
            self.snapshot_geometry_paths.clear();
            // A full re-extract supersedes any pending scoped spawn splice.
            self.snapshot_topology_splice = None;

            // Diagnostic (always-on, rate-limited): attribute the Quest
            // full-extract rate to a concrete per-frame cause. If this logs at
            // roughly the extract rate with `has_topology=false` and only
            // `TRANSFORM`/`INSPECTION` domains, the forcing cause is a non-patchable
            // skinned subtree; non-transform domains or `has_topology=true` point
            // elsewhere. Rate-limited so a sustained slow frame rate does not flood
            // logcat.
            self.tick_force_extract_count += 1;
            if self.tick_force_extract_count % 30 == 1 {
                const ALLOWED: u32 =
                    DirtyDomains::TRANSFORM.bits() | DirtyDomains::INSPECTION.bits();
                let bits = batch
                    .changes
                    .iter()
                    .fold(0u32, |acc, c| acc | c.domains.bits());
                let any_resync = batch.changes.iter().any(|c| c.resync);
                tracing::info!(
                    count = self.tick_force_extract_count,
                    has_topology,
                    any_resync,
                    batch_len = batch.changes.len(),
                    domain_bits = bits,
                    has_transform = (bits & DirtyDomains::TRANSFORM.bits()) != 0,
                    has_non_transform = (bits & !ALLOWED) != 0,
                    "extract-cause: tick forced a full re-extract (no patch path)"
                );
            }
        }
        if has_topology || batch.changes.iter().any(|change| change.domains.contains(DirtyDomains::METADATA)
            || change.property_name.as_deref().is_some_and(|name| name == "idtx:behavior:context" || name == "customData")) {
            // Scope the per-prim behavior-data FFI scan to the spawned subtree on a
            // localized topology change; whole-stage otherwise.
            match &topo_scope {
                Some(scope) => self.refresh_behavior_data_scoped(scope),
                None => self.refresh_behavior_data(),
            }
        }
        if has_topology || batch.changes.iter().any(|change| change.property_name.as_deref().is_some_and(|name|
            name.starts_with("idtx:behavior:") || name.starts_with("idtx:stageBehavior") || name == "idtx:playerRole")) {
            self.hydrator.reconcile_authored_behaviors(&self.service);
            match &topo_scope {
                Some(scope) => {
                    self.hydrator.hydrate_player_roles_scoped(&self.service, scope);
                }
                None => {
                    self.hydrator.hydrate_player_roles(&self.service);
                }
            }
            self.behavior_reconcile_pending = true;
        }
        #[cfg(feature = "physics")]
        {
            // A physics *structural / authoring* change — new/removed prims, or an
            // edit to a `physics:*` / `idtx:collision:*` property — may alter the
            // body set or a collider's schema, so re-hydrate the collision /
            // rigid-body components and request whole-stage in-place reconciliation.
            let physics_props_changed = batch.changes.iter().any(|change| {
                change.property_name.as_deref().is_some_and(|name| {
                    (name.starts_with("physics:") && name != "physics:startsAsleep")
                        || name.starts_with("idtx:collision:")
                })
            });
            let physics_structural = has_topology || physics_props_changed;
            // A post-hydration empty physics scope is not enough: an API/body
            // may have disappeared, or the lazy initial build may still be due.
            // Acknowledge only proven additive, neutral spawns when the preceding
            // generation was reconciled. Never overwrite a pending invalidation.
            let localized_no_physics = physics_was_current
                && !physics_props_changed
                && physics_topology_change
                    .as_ref()
                    .is_some_and(|change| change.is_physics_neutral_addition())
                && matches!(&topo_scope, Some(scope) if !self.hydrator.scope_has_physics(scope));
            if localized_no_physics {
                self.physics_build_gen = Some(self.topology_generation);
            } else if physics_structural {
                self.physics_build_gen = None;
                match &topo_scope {
                    Some(scope) => {
                        self.hydrator.hydrate_collision_scoped(&self.service, scope);
                        self.hydrator.hydrate_collision_bodies_scoped(&self.service, scope);
                        self.hydrator.hydrate_rigid_bodies_scoped(&self.service, scope);
                    }
                    None => {
                        self.hydrator.hydrate_collision(&self.service);
                        self.hydrator.hydrate_collision_bodies(&self.service);
                        self.hydrator.hydrate_rigid_bodies(&self.service);
                    }
                }
            } else {
                // Value-only geometry edit (e.g. an `OpenExec`-driven cube `size`):
                // the collision / rigid-body *components* are unchanged — only the
                // prim's `BoundsComp` moved, and the viewport reprojection above has
                // already refreshed it. So the two whole-stage `hydrate_*` scans are
                // unnecessary; queue the physics-relevant changed prims for an
                // incremental per-body reconcile in `tick_physics` instead of nulling
                // `physics_build_gen` (which would force a whole-stage rebuild every
                // interval — the per-second "built runtime physics world" hitch).
                // A non-collider geometry edit matches nothing here → zero physics work.
                for change in &batch.changes {
                    if change.domains.contains(DirtyDomains::GEOMETRY)
                        && self.hydrator.prim_has_collision(change.prim)
                    {
                        self.physics_dirty_prims.push(change.prim);
                    }
                }
            }
        }
        self.change_generation = self.change_generation.wrapping_add(1);

        // Keep the collision BVH in step with the stage after any change
        // (topology or value-only). Rebuilding reads live `BoundsComp`, so the
        // BVH never drifts from what the linear scan would see.
        #[cfg(feature = "collision-bvh")]
        self.refresh_collision_bvh();

        // Outbound collab: publish root transforms and concrete defaults.
        #[cfg(feature = "remote")]
        self.flush_outbound_collab(&batch);
        #[cfg(all(feature = "remote", feature = "authoring"))]
        self.flush_outbound_attributes(&batch)?;

        // Inbound collab: apply any remote edits queued by the background thread.
        #[cfg(all(feature = "remote", feature = "authoring"))]
        self.apply_inbound_collab()?;

        Ok(true)
    }

    // ── Collab hooks ─────────────────────────────────────────────────────────

    /// Broadcast locally-authored TRANSFORM changes to the collaboration server.
    ///
    /// Skips paths that were authored by an inbound remote edit (echo guard).
    #[cfg(feature = "remote")]
    fn flush_outbound_collab(&mut self, batch: &StageChangeBatch) {
        if self.collab.is_none() {
            return;
        }

        // Reset this tick's outbound counter.
        #[cfg(feature = "metrics")]
        {
            self.collab_tick_metrics = Some(CollabTickMetrics::default());
        }

        // A guard belongs to this notice batch, including resyncs. Compare the
        // applied value so an intervening local edit is never suppressed merely
        // because it touches the same path as an earlier remote edit.
        let remote_transforms = std::mem::take(&mut self.pending_remote_transforms);

        // Collect paths of locally-authored TRANSFORM changes.
        let paths: Vec<String> = batch
            .changes
            .iter()
            .filter(|c| {
                c.authored_in_root_layer && !c.resync && c.domains.contains(DirtyDomains::TRANSFORM)
            })
            .filter_map(|c| self.service.path_for(c.prim).map(str::to_owned))
            .collect();

        for path in paths {
            if let Some(remote) = remote_transforms.get(&path)
                && self.get_local_transform_f64(&path).as_ref() == Some(remote) {
                continue;
            }

            // Prefer the exact authored TRS for pure translate/rotateXYZ/scale
            // prims. Baking to a matrix and letting the server decompose it swaps
            // the Y and Z scale axes under a ±90° rotation (a non-uniform scale
            // combined with an axis-aligned quarter turn decomposes ambiguously).
            // Sending the authored Euler + per-axis scale verbatim means the
            // server never decomposes, so no swap can occur. The bug only
            // surfaces after the server persists the stage and it is reopened —
            // during live editing the local stack keeps the exact ops.
            //
            // Rotation is Euler degrees, XYZ order; translation and scale match
            // the LOCAL op values (not world accumulation), consistent with the
            // matrix fallback below.
            #[cfg(feature = "authoring")]
            if let Some((translation, rotation_deg, scale)) = self.authored_trs_for_outbound(&path)
            {
                if let Some(c) = &self.collab {
                    let queued = c.send_transform_separate(path, translation, rotation_deg, scale);
                    #[cfg(feature = "authoring")]
                    if !queued && let Some(state) = &mut self.collab_persistence { state.local_only_dirty = true; }
                    #[cfg(not(feature = "authoring"))]
                    let _ = queued;
                    #[cfg(feature = "metrics")]
                    if let Some(m) = &mut self.collab_tick_metrics {
                        m.outbound_edits += 1;
                    }
                }
                continue;
            }

            // Fallback: a matrix-op or otherwise non-TRS prim. Broadcast the
            // prim's LOCAL transform, matching the reference idtx-flow-client
            // (`GetLocalTransformation`). The wire matrix is the prim's local
            // xform op, not its world-space accumulation, so peers with
            // non-identity parents place it correctly.
            if let Some(usd_mat) = self.get_local_transform_f64(&path) {
                if let Some(c) = &self.collab {
                    let queued = c.send_transform(path, usd_mat);
                    #[cfg(feature = "authoring")]
                    if !queued && let Some(state) = &mut self.collab_persistence { state.local_only_dirty = true; }
                    #[cfg(not(feature = "authoring"))]
                    let _ = queued;
                    #[cfg(feature = "metrics")]
                    if let Some(m) = &mut self.collab_tick_metrics {
                        m.outbound_edits += 1;
                    }
                }
            }
        }
    }

    /// Return the exact authored TRS (translation, Euler-XYZ **degrees**, scale)
    /// for a prim whose xform stack is a pure translate/`rotateXYZ`/scale set,
    /// for the decompose-free outbound collab path ([`flush_outbound_collab`]).
    ///
    /// Returns `None` for matrix, inverse/pivot, animated-only,
    /// `orient`/non-`rotateXYZ`, or non-editable stacks — those fall back to the
    /// baked-matrix broadcast. Mirrors the shell's `XformEditKind::TrsOps`
    /// classification (`classify_authored_transform`) so the wire representation
    /// matches exactly what the inspector treats as exact.
    ///
    /// [`flush_outbound_collab`]: Self::flush_outbound_collab
    #[cfg(all(feature = "remote", feature = "authoring"))]
    fn authored_trs_for_outbound(&mut self, path: &str) -> Option<([f64; 3], [f64; 3], [f64; 3])> {
        let stack = self.authored_xform_stack(path)?;
        trs_ops_for_outbound(&stack.ops, stack.editable)
    }

    /// Author the server-authored attribute values for a remote-created built-in
    /// prim. Only the values present in the broadcast (has-field semantics) are
    /// authored; the next tick's value drain hydrates them. A failure prevents
    /// the stage owner from acknowledging this collaboration revision.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    fn author_remote_create_attributes(
        &mut self,
        prim_path: &str,
        size: Option<f64>,
        height: Option<f64>,
        radius: Option<f64>,
        axis: Option<&str>,
        width: Option<f64>,
        length: Option<f64>,
    ) -> Result<(), SessionError> {
        use idtx_usd::PropertyValue;
        let attrs = [
            ("size", size.map(PropertyValue::F64)),
            ("height", height.map(PropertyValue::F64)),
            ("radius", radius.map(PropertyValue::F64)),
            ("axis", axis.map(|a| PropertyValue::Token(a.to_owned()))),
            ("width", width.map(PropertyValue::F64)),
            ("length", length.map(PropertyValue::F64)),
        ];
        for (property, value) in attrs {
            let Some(value) = value else { continue };
            self.author_attribute(prim_path, property, value)?;
        }
        Ok(())
    }

    /// Author the server-authored **local** transform of a remote-created prim
    /// verbatim (relative to its parent).
    ///
    /// The updated idtx-core contract authors the `CreatePrim` /
    /// `CreatePrimBroadcast` transform directly into the layer as a local
    /// `xformOp`, so there is no world→local conversion to undo: the matrix is
    /// authored exactly as received.
    ///
    /// No-op when `transform` is `None`: the prim then inherits its parent's
    /// transform (identity local), matching the server's "created at the
    /// parent's transform" contract.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    fn apply_remote_create_transform(
        &mut self,
        path: &str,
        transform: Option<idtx_collab::client::RemoteWorldTransform>,
    ) -> Result<(), SessionError> {
        let Some(local) = Self::remote_create_local_matrix(transform.as_ref())? else { return Ok(()) };
        // NO echo guard here (unlike the inbound TRANSFORM edit path): this
        // authors the *first* xformOp on a just-created, op-less prim, which USD
        // records as a RESYNC (spec creation). `flush_outbound_collab` filters on
        // `!resync`, so this transform is never broadcast back in the first place.
        self.author_matrix_transform(path, local)
    }

    /// The flat row-major USD **local** matrix to author for a remote create
    /// broadcast transform, or `None` when the broadcast carried none. Pure
    /// wire→USD conversion with no stage reads, so it can be computed before the
    /// prim exists: the create path calls it first as a fail-fast check, so a
    /// malformed wire transform errors before any stage mutation.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    fn remote_create_local_matrix(
        transform: Option<&idtx_collab::client::RemoteWorldTransform>,
    ) -> Result<Option<[f64; 16]>, SessionError> {
        Ok(transform.map(remote_world_to_usd_mat).transpose()?)
    }

    #[cfg(all(feature = "remote", feature = "authoring"))]
    fn remember_remote_transform(&mut self, path: &str) -> Result<(), SessionError> {
        let prim = self.service.prim_at_path(path).ok_or_else(|| StageError::UnknownPath(path.into()))?;
        let matrix = self.service.get_local_transform_f64(prim)?;
        self.pending_remote_transforms.insert(path.into(), matrix);
        Ok(())
    }

    /// Author a server-authored prim active-state change into the root layer.
    ///
    /// `active = false` is a soft delete; `active = true` restores it.
    ///
    /// The two directions take deliberately different follow-up paths:
    ///
    /// - **`active = false`** mirrors [`delete_prim`][Self::delete_prim]: a full
    ///   `project_topology()` (which traverses only *active* prims) drops the
    ///   now-inactive subtree from the topology store, so — as in `delete_prim` —
    ///   the next tick's drain-diff has no baseline to see the removal. The ECS
    ///   subtree is therefore despawned explicitly and a fresh extract forced, so
    ///   the prim and its descendants leave the scene tree and stop rendering
    ///   immediately. (Relying on the drain-diff here is also unsound: a rooted
    ///   reprojection of an inactive root is an `UsdPrimRange` edge case, whereas
    ///   the full active-only traverse is unambiguous.)
    ///
    /// - **`active = true`** refreshes the subtree immediately so subsequent
    ///   transform/attribute frames in the same snapshot can address its prims.
    ///
    /// Idempotent: re-authoring the current state (e.g. a reconnect snapshot
    /// replay) is a no-op write that produces no change notice.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    fn author_remote_set_active(&mut self, path: &str, active: bool) -> Result<(), SessionError> {
        self.service
            .apply_structural_command(&idtx_usd::StructuralCommand::SetActive {
                path: path.to_owned(),
                active,
            })?;
        if !active {
            self.service.project_topology()?;
            self.hydrator.despawn_subtree(path);
            self.snapshot_dirty = true;
            self.topology_generation = self.topology_generation.wrapping_add(1);
            // The selected prim may be the one just pruned, or a descendant of
            // it. Only now — once the server-authored deletion is actually
            // applied — is it safe to drop the selection (a rejected request
            // never reaches this path, so the selection survives it).
            if self
                .selected_path
                .as_deref()
                .is_some_and(|sel| path_in_subtree(sel, path))
            {
                self.selected_path = None;
            }
        } else {
            let delta = self.service.project_topology_paths(&[path.to_owned()])?;
            self.hydrator.apply_topology_delta(&delta, &self.service);
            self.snapshot_dirty = true;
            self.topology_generation = self.topology_generation.wrapping_add(1);
            self.replay_deferred_attributes()?;
        }
        Ok(())
    }

    /// Apply and confirm the ordered inbound prefix. Failed and later envelopes
    /// stay queued and synchronization stops until explicit recovery (S11).
    #[cfg(all(feature = "remote", feature = "authoring"))]
    fn apply_inbound_collab(&mut self) -> Result<(), SessionError> {
        if self.collab.as_ref().is_some_and(|client| !client.is_connected() || !client.is_synced()) {
            self.discard_attribute_predictions()?;
        }
        #[cfg(feature = "metrics")]
        let t_inbound = std::time::Instant::now();
        #[cfg(feature = "metrics")]
        let mut inbound_count: u64 = 0;
        #[cfg(feature = "metrics")]
        let mut inbound_separate: u64 = 0;
        #[cfg(feature = "metrics")]
        let mut inbound_matrix: u64 = 0;

        let started = std::time::Instant::now();
        let mut applied = 0;
        while let Some(edit) = self.collab.as_ref().and_then(|c| c.next_inbound()) {
            if applied >= 16 || started.elapsed() >= std::time::Duration::from_millis(2) { break; }
            // Prepare the complete referenced stage on a worker before authoring
            // the arc. Keep this handle alive through composition/application.
            let _prepared = if let idtx_collab::client::RemoteEditKind::CreateReference { reference_path, .. } = &edit.kind {
                if self.collab_preparation.as_ref().is_none_or(|job| job.path != *reference_path) {
                    match collab_prepare::Preparation::start(&self.service.metadata().root_identifier, reference_path) {
                        Ok(job) => self.collab_preparation = Some(job),
                        Err(error) => {
                            self.collab.as_ref().expect("collab").fail_inbound();
                            self.push_collab_notice(format!("Collaboration asset preparation stopped: {error}"));
                            return Err(error);
                        }
                    }
                    break;
                }
                let Some(result) = self.collab_preparation.as_ref().and_then(|job| job.poll()) else { break; };
                self.collab_preparation = None;
                match result {
                    Ok(stage) => Some(stage),
                    Err(error) => {
                        self.collab.as_ref().expect("collab").fail_inbound();
                        self.push_collab_notice(format!("Collaboration asset preparation stopped: {error}"));
                        return Err(error);
                    }
                }
            } else { None };
            let create = matches!(edit.kind, idtx_collab::client::RemoteEditKind::CreatePrim { .. }
                | idtx_collab::client::RemoteEditKind::CreateReference { .. });
            let before = if create { Some(self.service.root_prim_text(&edit.prim_path)?) } else { None };
            if before.as_ref().is_some_and(|text| !self.can_retain_collab_opinion(text)) {
                self.collab.as_ref().expect("collab").fail_inbound();
                self.push_collab_notice("Recovery capacity reached; export recovery and reconcile before reopening.".into());
                return Err(SessionError::Collaboration("recovery capacity reached".into()));
            }
            applied += 1;
            let mut create_delta = None;
            self.collab_applying = true;
            let result = (|| -> Result<(), SessionError> {
            let prim_path = edit.prim_path.clone();
            if matches!(edit.kind, idtx_collab::client::RemoteEditKind::Matrix(_)
                | idtx_collab::client::RemoteEditKind::Separate { .. })
                && self.service.prim_at_path(&prim_path).is_none() {
                return Err(SessionError::Collaboration(format!("transform target does not exist: {prim_path}")));
            }

            // The server prefers a decomposed SeparateTransform and only falls
            // back to a full matrix. Apply each verbatim: the Separate path keeps
            // the wire TRS directly (no Euler round-trip through a rebuilt
            // matrix); general matrices are authored directly without decomposition.
            let (translation, rotation_xyz_degrees, scale) = match edit.kind.clone() {
                idtx_collab::client::RemoteEditKind::SetAttributes { attributes } => {
                    if self.defer_missing_snapshot_attributes(&prim_path, edit.server_seq, &attributes)? { return Ok(()); }
                    self.apply_remote_attributes(&prim_path, edit.server_seq, &attributes)?;
                    #[cfg(feature = "metrics")]
                    { inbound_count += 1; }
                    return Ok(());
                }
                idtx_collab::client::RemoteEditKind::AttributesAcknowledged { request_id, failed, attributes } => {
                    self.complete_attribute_request(&prim_path, edit.server_seq, request_id, &failed, &attributes)?;
                    return Ok(());
                }
                idtx_collab::client::RemoteEditKind::Invalid { reason } => return Err(SessionError::Collaboration(reason)),
                idtx_collab::client::RemoteEditKind::SnapshotComplete => {
                    self.confirm_attribute_snapshot_targets()?;
                    return Ok(());
                }
                idtx_collab::client::RemoteEditKind::Acknowledged { .. } => return Ok(()),
                idtx_collab::client::RemoteEditKind::LayerReloaded { usd_path, usd_uri } => {
                    self.discard_attribute_predictions()?;
                    // A file this stage uses was reloaded on the server (another
                    // session's commit, or a replacing upload). We cannot re-fetch
                    // and reopen the layer from inside the session (the HTTP
                    // resolver's force-refresh and the tab lifecycle live in the
                    // shell), so record it and let the shell drive the re-fetch +
                    // in-place layer reload + fresh snapshot. Applying nothing here
                    // keeps the ordered prefix advancing; stage frames before the
                    // post-reload snapshot are superseded by it.
                    self.pending_layer_reloads.push(PendingLayerReload { usd_path, usd_uri });
                    return Ok(());
                }
                idtx_collab::client::RemoteEditKind::Matrix(matrix) => {
                    #[cfg(feature = "metrics")]
                    {
                        inbound_matrix += 1;
                    }
                    self.author_matrix_transform(&prim_path, matrix)?;
                    self.remember_remote_transform(&prim_path)?;
                    #[cfg(feature = "metrics")]
                    { inbound_count += 1; }
                    return Ok(());
                }
                idtx_collab::client::RemoteEditKind::Separate {
                    translation,
                    rotation_deg,
                    scale,
                } => {
                    #[cfg(feature = "metrics")]
                    {
                        inbound_separate += 1;
                    }
                    // Only canonical TRS stacks have equivalent per-op writes.
                    // A pivot/orient/reordered/animated stack needs the full
                    // USD-canonical local matrix, just like a wire matrix.
                    if self.authored_trs_for_outbound(&prim_path).is_none() {
                        let matrix = idtx_usd::usd_trs_matrix(idtx_usd::TransformValues {
                            translation, rotation_xyz_degrees: rotation_deg, scale,
                        })?;
                        self.author_matrix_transform(&prim_path, matrix)?;
                        self.remember_remote_transform(&prim_path)?;
                        #[cfg(feature = "metrics")]
                        { inbound_count += 1; }
                        return Ok(());
                    }
                    (
                        [translation[0], translation[1], translation[2]],
                        [rotation_deg[0], rotation_deg[1], rotation_deg[2]],
                        [scale[0], scale[1], scale[2]],
                    )
                }
                idtx_collab::client::RemoteEditKind::CreatePrim {
                    type_name,
                    size,
                    height,
                    radius,
                    axis,
                    width,
                    length,
                    world,
                } => {
                    Self::remote_create_local_matrix(world.as_ref())?;
                    create_delta = Some(self.service.reconcile_root_prim(&prim_path, &type_name, "", false)?);
                    self.author_remote_create_attributes(
                        &prim_path, size, height, radius, axis.as_deref(), width, length,
                    )?;
                    self.remember_remote_create_attributes(&prim_path)?;
                    self.apply_remote_create_transform(&prim_path, world)?;
                    self.remember_remote_transform(&prim_path)?;
                    #[cfg(feature = "metrics")]
                    {
                        inbound_count += 1;
                    }
                    return Ok(());
                }
                idtx_collab::client::RemoteEditKind::CreateReference {
                    as_payload,
                    reference_path,
                    world,
                } => {
                    Self::remote_create_local_matrix(world.as_ref())?;
                    create_delta = Some(self.service.reconcile_root_prim(&prim_path, "", &reference_path, as_payload)?);
                    self.apply_remote_create_transform(&prim_path, world)?;
                    // Typeless/Scope assets may have no transform at all.
                    if self.service.prim_at_path(&prim_path).is_some_and(|prim|
                        self.service.get_local_transform_f64(prim).is_ok()) {
                        self.remember_remote_transform(&prim_path)?;
                    }
                    #[cfg(feature = "metrics")]
                    {
                        inbound_count += 1;
                    }
                    return Ok(());
                }
                idtx_collab::client::RemoteEditKind::SetActive { active } => {
                    // A server-authored active-state change (our own echo, a
                    // peer's soft delete / undo, or a join-snapshot replay).
                    // Author `active` into the root layer; the next tick's drain
                    // picks up the resync and (de)hydrates the subtree — active=
                    // false prunes it from the scene tree + render, active=true
                    // re-adds and hydrates it. See `author_remote_set_active`.
                    self.author_remote_set_active(&prim_path, active)?;
                    #[cfg(feature = "metrics")]
                    {
                        inbound_count += 1;
                    }
                    return Ok(());
                }
            };

            // Mark this prim so the outbound hook skips its echo in the next tick
            // (also suppresses re-broadcast of server corrections).
            self.author_transform(&prim_path, translation, rotation_xyz_degrees, scale)?;
            self.remember_remote_transform(&prim_path)?;

            #[cfg(feature = "metrics")]
            {
                inbound_count += 1;
            }
            Ok(())
            })();
            self.collab_applying = false;
            let result = if let Some(before) = before {
                if result.is_err() {
                    // Restore only this subtree, including absence. Keep notices
                    // so projection caches see the rollback as well as the edit.
                    let rollback = self.service.replace_root_prim_text(&edit.prim_path, &before)
                        .and_then(|()| self.service.project_topology_paths(std::slice::from_ref(&edit.prim_path)));
                    match rollback {
                        Ok(delta) => {
                            self.hydrator.apply_topology_delta(&delta, &self.service);
                            result
                        }
                        Err(error) => Err(error.into()),
                    }
                } else {
                    if let Some(delta) = create_delta {
                        // Consume removals now: a second projection cannot
                        // recover tombstones already consumed by the first one.
                        self.hydrator.apply_topology_delta(&delta, &self.service);
                        self.snapshot_dirty = true;
                        self.topology_generation = self.topology_generation.wrapping_add(1);
                    }
                    if !before.is_empty() && self.service.root_prim_text(&edit.prim_path)? != before {
                        self.collab_recovery.push((edit.prim_path.clone(), before));
                        self.push_collab_notice(format!("Preserved pre-replay opinions for {} in collaboration recovery", edit.prim_path));
                    }
                    result
                }
            } else { result };
            if result.is_err() { self.observe_collab_root(true)?; }
            if let Some(client) = &self.collab {
                if let Err(error) = result {
                    client.fail_inbound();
                    self.push_collab_notice(format!("Collaboration application stopped at revision {}: {error}", edit.server_seq));
                    return Err(error);
                }
                client.acknowledge_inbound(&edit);
            }
        }

        self.observe_collab_root(true)?;
        #[cfg(feature = "metrics")]
        if let Some(m) = &mut self.collab_tick_metrics {
            m.inbound_edits = inbound_count;
            m.inbound_separate = inbound_separate;
            m.inbound_matrix = inbound_matrix;
            m.inbound_apply_us = u64::try_from(t_inbound.elapsed().as_micros()).unwrap_or(u64::MAX);
        }

        // Drain protocol events once: fold them into user-facing notices (always)
        // and this tick's metrics (when enabled).
        let events = match &self.collab {
            Some(c) => c.drain_events(),
            None => Vec::new(),
        };
        for ev in events {
            match &ev {
                idtx_collab::client::CollabEvent::CreateRejected {
                    parent_path,
                    type_name,
                    reason,
                } => {
                    self.push_collab_notice(format!(
                        "Server rejected creating {type_name} under {parent_path}: {reason}"
                    ));
                }
                idtx_collab::client::CollabEvent::SetActiveRejected {
                    prim_path,
                    active,
                    reason,
                } => {
                    let verb = if *active { "restoring" } else { "deleting" };
                    self.push_collab_notice(format!("Server rejected {verb} {prim_path}: {reason}"));
                }
                idtx_collab::client::CollabEvent::Error { code, message } => {
                    self.push_collab_notice(format!("Collaboration error [{code}]: {message}"));
                }
                idtx_collab::client::CollabEvent::Ack { .. } => {}
            }

            #[cfg(feature = "metrics")]
            if let Some(m) = &mut self.collab_tick_metrics {
                match &ev {
                    idtx_collab::client::CollabEvent::Ack { ok: false, error, .. } => {
                        if error == "stale" {
                            m.stale_acks += 1;
                        } else {
                            m.rejected_acks += 1;
                        }
                    }
                    idtx_collab::client::CollabEvent::Ack { ok: true, .. } => {}
                    idtx_collab::client::CollabEvent::Error { .. }
                    | idtx_collab::client::CollabEvent::CreateRejected { .. }
                    | idtx_collab::client::CollabEvent::SetActiveRejected { .. } => {
                        m.rejected_acks += 1;
                    }
                }
            }
        }

        Ok(())
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────────

    /// Records the first-render duration, computes the total open-to-frame time,
    /// and writes a JSON metrics file to the system temp directory. Only available with `metrics` feature.
    #[cfg(feature = "metrics")]
    pub fn finalize_open_metrics(&mut self, first_render_elapsed: std::time::Duration) {
        if let Some(m) = &mut self.open_metrics {
            m.first_render_ms = first_render_elapsed.as_secs_f64() * 1000.0;
            m.total_ms = self.t_open_start.elapsed().as_secs_f64() * 1000.0;
            if let Err(e) = m.write_json() {
                eprintln!("[idtx-metrics] failed to write JSON: {e}");
            }
        }
    }

    // ── Physics (runtime-only, UsdPhysicsScene-gated) ──────────────────────────

    /// Step the runtime rigid-body physics simulation by `dt` seconds and author
    /// the resulting dynamic poses into the session layer.
    ///
    /// A **no-op** unless the hydrated stage contains a `UsdPhysicsScene` prim
    /// (the sole activation gate — plan §3). On the first call after a stage open
    /// (and after any topology change) the subsystem is (re)built from the
    /// authored `UsdPhysics*` schemas; thereafter each call:
    ///
    /// 1. feeds every **kinematic** body its live ECS world pose (set this frame
    ///    by [`Self::tick_animation`] / [`Self::tick_behaviors`]) as the next
    ///    kinematic goal, so pushed dynamics respond (plan §6.3);
    /// 2. advances the fixed-timestep solver (spiral-of-death substep clamp);
    /// 3. authors every **dynamic** body's world→local pose into the session
    ///    layer in **one** bulk [`StageService::author_session_transforms`] call,
    ///    then runs **one** transform-only re-projection ([`Self::tick`]) —
    ///    honouring the "bulk, not per-prim FFI" invariant.
    ///
    /// Call it in the runtime frame loop **after** `tick_animation` + `tick_behaviors`
    /// and **before** `render_one_frame` / `render_xr` (plan §6.1), so the solver
    /// integrates against this frame's kinematic goals and the render reads the
    /// result. It is compiled only under the `physics` feature (runtime-only).
    ///
    /// `defer_reprojection` skips step 3's `self.tick()` drain, authoring the poses
    /// but leaving them for a later drain this same frame. The XR path passes
    /// `true`: `render_xr` already runs exactly one `drain_changes` + reprojection
    /// after it authors the controller/pointer poses, so a single drain can cover
    /// both the physics and controller writes — halving the per-frame USD-side cost
    /// that was previously two full drain+reproject passes. The desktop/headless
    /// path passes `false` (physics owns the sole drain there).
    #[cfg(feature = "physics")]
    pub fn tick_physics(&mut self, dt: f32, defer_reprojection: bool) -> Result<(), SessionError> {
        // Build once, lazily. Detection returns `None` when no UsdPhysicsScene is
        // present → simulation stays off; we retry on each topology change so a
        // scene introduced by a later payload load still activates. Once built,
        // the world persists across frames so velocity accumulates. Reconcile
        // topology/configuration changes in place: unchanged bodies keep their
        // solver state; changed colliders retain motion unless explicitly reset
        // by an authored velocity/kind change.
        if self.physics_build_gen != Some(self.topology_generation) {
            #[cfg(feature = "physics-metrics")]
            let started = std::time::Instant::now();
            self.physics = physics::detect_and_build(&mut self.service, &self.hydrator, self.physics.take());
            #[cfg(feature = "physics-metrics")]
            tracing::debug!(elapsed_us = started.elapsed().as_micros() as u64, "physics reconciliation");
            self.physics_build_gen = Some(self.topology_generation);
            // Whole-stage reconciliation re-reads every body, so any incremental dirty
            // set is already incorporated — drop it.
            self.physics_dirty_prims.clear();
            // Replay any grab commands posted before the subsystem existed (e.g. a
            // first-frame grab), now that it is built — see `apply_physics_control`.
            if self.physics.is_some() {
                if !self.pending_physics_control.is_empty() {
                    let pending = std::mem::take(&mut self.pending_physics_control);
                    self.apply_physics_control(&pending);
                }
            } else {
                // No `UsdPhysicsScene` → nothing will ever consume these; drop them
                // rather than buffer unbounded (grab is a pure no-op without a sim).
                self.pending_physics_control.clear();
            }
        } else if !self.physics_dirty_prims.is_empty() {
            // Incremental fast path: one or more collider prims had a value-only
            // geometry edit (e.g. the `OpenExec`-driven cube `size`). Re-bake ONLY
            // those bodies in place instead of a whole-stage rebuild — no full ECS
            // scan, and untouched bodies keep their live solver state. Falls back to
            // a full rebuild next frame if a dirty prim no longer maps to a tracked
            // body (a structural change that slipped through the value-only gate).
            let dirty = std::mem::take(&mut self.physics_dirty_prims);
            if let Some(sub) = self.physics.take() {
                let (sub, needs_full) =
                    physics::reconcile_dirty(&mut self.service, &self.hydrator, sub, &dirty);
                self.physics = Some(sub);
                if needs_full {
                    self.physics_build_gen = None;
                }
            }
            // If `self.physics` is `None` (no scene), the drained dirty set is dropped.
        }

        // Feed kinematic goals, step, and collect dynamic authoring entries while
        // the physics borrow is live; drop it before authoring (which borrows
        // `self.service` + `self.tick`). `self.physics` and `self.hydrator` are
        // disjoint fields, so the simultaneous &mut/& borrow is sound.
        let (entries, sleep_flags) = {
            let Some(sub) = self.physics.as_mut() else {
                return Ok(());
            };
            sub.feed_kinematic(&self.hydrator);
            sub.advance(dt);
            #[cfg(feature = "physics-metrics")]
            {
                let m = sub.metrics();
                tracing::debug!(
                    substeps = m.substeps_last_frame,
                    bodies = m.body_count,
                    advance_us = m.advance_micros,
                    "physics step"
                );
            }
            // Diagnostic (always-on, rate-limited): awake vs asleep dynamic bodies.
            // `asleep=0` on a visually-settled scene means the island never reaches
            // rest (e.g. a jittery complex-hull contact — the cookie's ~285-vtx
            // hull); a rising `awake`/`bodies` over the session means bodies
            // accumulate without settling. Grep logcat for `physics-sleep`.
            {
                use std::sync::atomic::{AtomicU64, Ordering};
                static N: AtomicU64 = AtomicU64::new(0);
                if N.fetch_add(1, Ordering::Relaxed) % 120 == 1 {
                    let (awake, asleep) = sub.dynamic_sleep_counts();
                    let (bodies, colliders) = sub.counts();
                    tracing::info!(awake, asleep, bodies, colliders, "physics-sleep");
                }
            }
            sub.collect_authoring(&self.hydrator)
        };

        // Settled-scene fast exit: no dynamic body moved or changed sleep state
        // this frame, so there is nothing to author or re-project — the sleep gate
        // makes a fully-rested scene cost one cheap `advance` and zero USD work
        // (plan Phase 2A). (A sleep/wake edge always emits a transform too, so an
        // empty `entries` implies an empty `sleep_flags`; both are checked for
        // clarity.)
        if entries.is_empty() && sleep_flags.is_empty() {
            return Ok(());
        }

        // Mirror sleep/wake transitions into the session layer so the authored
        // `physics:startsAsleep` tracks the live sim (rare — edges only). A write
        // failure here is non-fatal: it only desyncs the persisted rest flag, not
        // the running sim, so warn and continue to author the pose.
        for (path, asleep) in &sleep_flags {
            if let Err(e) = self.service.author_session_property(
                path,
                idtx_usd_extension::ATTR_PHYSICS_STARTS_ASLEEP,
                &idtx_usd::PropertyValue::Bool(*asleep),
            ) {
                tracing::warn!(%path, "author physics:startsAsleep: {e}");
            }
        }

        // Author the moved set in one lock / change block / notice batch.
        // Normally one transform-only re-projection then drives the ECS + render
        // snapshot — but on the XR path (`defer_reprojection`) we leave the drain
        // to `render_xr`, which reprojects the controller poses it authors in the
        // same frame, so both batches drain in a single pass.
        if !entries.is_empty() {
            self.service.author_session_transforms(&entries)?;
        }
        if !defer_reprojection {
            self.tick()?;
        }
        Ok(())
    }

    /// Drive the `OpenExec` dataflow one tick (feature `openexec`): drain any REST
    /// responses the poller threads delivered, author them onto their datasource
    /// `outputs:data`, run the `OpenExec` compute, and author every changed computed
    /// value into the session layer — then (unless deferred) one drain+reproject
    /// carries those writes to the ECS/render world.
    ///
    /// Mirrors [`Self::tick_physics`]: main-thread only (all stage mutation under
    /// the exclusive lifecycle lock), lazily builds the per-stage exec program and
    /// rebuilds it on topology change, and early-returns at zero cost when the
    /// stage carries no IDTX compute graph (no subsystem, no poller threads).
    ///
    /// `defer_reprojection` skips the `self.tick()` drain so the XR path can fold
    /// these writes into `render_xr`'s single per-frame drain (same convention as
    /// `tick_physics`); desktop/headless pass `false`.
    ///
    /// Returns the number of attributes authored this tick (datasource
    /// `outputs:data` writes plus changed computed-consumer writes) — `0` on the
    /// no-op path. A caller can treat a non-zero return as "the compute graph
    /// produced live data this tick" (e.g. the headless `--screenshot` capture
    /// waits for the first non-zero tick before reading pixels).
    ///
    /// # Errors
    /// Returns [`SessionError`] if the exec compute/author or the reprojection
    /// drain fails.
    #[cfg(feature = "openexec")]
    fn exec_sources_fingerprint(sources: &[idtx_usd::ExecDataSource]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        sources.len().hash(&mut hasher);
        for s in sources {
            s.prim_path.hash(&mut hasher);
            s.kind.hash(&mut hasher);
            s.endpoint.hash(&mut hasher);
            s.query.hash(&mut hasher);
            s.method.hash(&mut hasher);
            s.authorization.hash(&mut hasher);
            s.json_body.hash(&mut hasher);
            s.interval.to_bits().hash(&mut hasher);
        }
        hasher.finish()
    }

    #[cfg(feature = "openexec")]
    pub fn tick_exec(&mut self, defer_reprojection: bool) -> Result<u64, SessionError> {
        // Lazy build / rebuild on topology change. If transport must be replaced,
        // dropping the old subsystem cancels its pool and isolates late responses
        // on the old channel; exec_release retires the old C++ bridge before value
        // keys are re-registered. Reusing transport still refreshes program metadata.
        if self.exec_build_gen != Some(self.topology_generation) {
            // Tearing down the poll pool and re-registering every datasource
            // (per-source SSRF validation + DNS resolution + reqwest client build,
            // all on this thread) is what froze the stage on every runtime spawn —
            // the topology generation bumps, but the datasource set is unchanged.
            // So only rebuild the pool when that set actually differs; otherwise
            // keep the running pollers and just refresh the C++ program (a resync
            // may add/remove compute prims, so the program itself is rebuilt
            // either way). `exec_data_sources` is a read-only stage traversal — no
            // network, independent of the compiled program.
            let new_fp = Self::exec_sources_fingerprint(
                &self.service.exec_data_sources().unwrap_or_default(),
            );
            if self.exec.is_some() && new_fp == self.exec_sources_fp {
                if let Err(e) = self.service.exec_build() {
                    tracing::warn!("exec program refresh failed, rebuilding pool: {e}");
                    self.exec = None;
                    let _ = self.service.exec_release();
                    self.exec = exec::ExecSubsystem::detect_and_build(
                        &mut self.service,
                        &self.exec_network_policy,
                    );
                } else if let Some(subsystem) = self.exec.as_mut() {
                    subsystem.refresh_program_metadata(&self.service);
                }
            } else {
                self.exec = None;
                let _ = self.service.exec_release();
                self.exec = exec::ExecSubsystem::detect_and_build(
                    &mut self.service,
                    &self.exec_network_policy,
                );
            }
            self.exec_sources_fp = new_fp;
            self.exec_build_gen = Some(self.topology_generation);
            // A freshly (re)built program must evaluate once before the idle gate
            // can suppress it (see `exec_did_initial_tick`).
            self.exec_did_initial_tick = false;
        }

        // Drain the REST poller channel into the C++ pending queue, then run the
        // apply+compute+author tick under the exclusive lock. `self.exec` and
        // `self.service` are disjoint fields, so the shared/mut borrows are sound;
        // the `self.exec` borrow ends before the `self.tick()` below.
        let authored = {
            let Some(sub) = self.exec.as_ref() else {
                return Ok(0);
            };
            let drained = sub.drain_into(&self.service);
            // Idle gate (perf): a purely datasource-driven graph only changes when
            // a payload arrives. With nothing drained this frame AND no live
            // time/host-state provider (`Compute_Environment`), the compute would
            // produce no change — so skip `exec_tick` entirely and never acquire the
            // exclusive stage-lifecycle lock. A graph with a live provider (idcm
            // clocks / env / auth tokens) recomputes every frame as before. This
            // removes the per-frame compute + lock on the ~98% of frames that carry
            // no new datasource data.
            //
            // The gate is held off until the program has evaluated at least once
            // (`exec_did_initial_tick`): a static/datasource-free graph, or a
            // datasource graph before its first payload, has `drained == 0` and no
            // live provider, so gating it immediately would never let its authored
            // defaults / static computations reach consumers.
            if self.exec_did_initial_tick && drained == 0 && !sub.has_live_providers() {
                #[cfg(feature = "openexec-metrics")]
                tracing::debug!(drained, skipped = true, "exec tick (idle-gated)");
                return Ok(0);
            }
            #[cfg(feature = "openexec-metrics")]
            let t_exec = std::time::Instant::now();
            let authored = self.service.exec_tick()?;
            // The program has now evaluated once; the idle gate may engage.
            self.exec_did_initial_tick = true;
            #[cfg(feature = "openexec-metrics")]
            tracing::debug!(
                drained,
                authored,
                skipped = false,
                compute_us = t_exec.elapsed().as_micros() as u64,
                live_providers = sub.has_live_providers(),
                "exec tick"
            );
            // Pull the datasource inputs the compute graph just resolved (a
            // connected `authorization` / `jsonBody`, e.g. an `${auth:TOKEN}`) into
            // each poller's live cell, so the next poll carries the fresh value
            // without any stage authoring. Cheap no-op when no input is connected.
            sub.refresh_inputs(&self.service);
            authored
        };

        if authored > 0 && !defer_reprojection {
            self.tick()?;
        }
        Ok(authored)
    }

    // ── Animation ─────────────────────────────────────────────────────────────

    /// Advance the animation clock by `dt_seconds` and, if the clock moved,
    /// re-project all animated prims at the new time code.
    ///
    /// Call this every frame loop iteration (after `tick()`, before
    /// `render_one_frame()`).  When the clock is paused and already
    /// sampled, this is a no-op.
    pub fn tick_animation(&mut self, dt_seconds: f64) -> Result<(), SessionError> {
        // Per-entity clip players (`AnimClipPlayer`) are transported
        // independently of the stage-global clock and carry their own target
        // subtrees, so advance + resample them first — this runs even when there
        // is no `pseudo_root` / `StagePlayback` (a level may drive gameplay clips
        // without a stage authoring timeline).
        self.tick_clip_animation(dt_seconds)?;

        let Some(root) = self.pseudo_root else {
            return Ok(());
        };

        // Advance the clock.
        if let Some(mut clock) = self
            .hydrator
            .world_mut()
            .get_mut::<idtx_scene::StagePlayback>(root)
        {
            clock.advance(dt_seconds);
        }

        // Check if a resample is needed.
        let needs_sample = self
            .hydrator
            .world()
            .get::<idtx_scene::StagePlayback>(root)
            .is_some_and(|c| c.playing || c.sample_pending());

        if !needs_sample {
            return Ok(());
        }

        let tc = {
            let Some(clock) = self.hydrator.world().get::<idtx_scene::StagePlayback>(root) else {
                return Ok(());
            };
            clock.current_time_code
        };

        // Collect animated prim IDs and split by animation domain.
        // Everything from here through ECS write-back is timed under the
        // `metrics` feature as `last_anim_sample_ns`.
        #[cfg(feature = "metrics")]
        let t_anim_sample = std::time::Instant::now();

        let (xform_only_ids, skel_pose_ids, full_ids) =
            idtx_scene::animation::classify_animated_prim_ids(self.hydrator.world());

        // Fast path: prims with only xformOp.* tracks need just a world-matrix
        // update — skip full geometry/material/bounds re-extraction.
        let xform_patches = if xform_only_ids.is_empty() {
            None
        } else {
            let transforms = self.service.sample_transforms_only(tc, &xform_only_ids)?;
            Some(self.hydrator.apply_transform_samples(&transforms)?)
        };

        if xform_only_ids.is_empty() && skel_pose_ids.is_empty() && full_ids.is_empty() {
            // No per-prim animation yet — project the full stage.
            let projection = self.service.sample_viewport(tc)?;
            self.hydrator.apply_viewport_projection(&projection)?;
            self.snapshot_dirty = true;
            self.snapshot_transform_patches = None;
        } else if !full_ids.is_empty() {
            // Slow path: prims with geometry-affecting animated attributes
            // (animated points, visibility, blend shapes, …) need a full
            // viewport resample. Skeleton-pose prims, if any, are folded in so
            // their palettes are resampled at the same time.
            let mut roots = full_ids.clone();
            roots.extend_from_slice(&skel_pose_ids);
            // Name the prims (and their animated attributes) that forced the full
            // path, so the classifier can be extended to cover cheap cases.
            #[cfg(feature = "metrics")]
            {
                use idtx_scene::{AnimatedAttribute, PrimNode, TrackTarget};
                let world = self.hydrator.world();
                for e in world.iter_entities() {
                    let ent = e.id();
                    let (Some(node), Some(anim)) = (
                        world.get::<PrimNode>(ent),
                        world.get::<AnimatedAttribute>(ent),
                    ) else {
                        continue;
                    };
                    if !full_ids.contains(&node.prim) {
                        continue;
                    }
                    let attrs: Vec<String> = anim
                        .tracks
                        .iter()
                        .map(|t| match &t.target {
                            TrackTarget::Attribute { attr_name } => attr_name.clone(),
                            TrackTarget::SkeletalJoint { joint } => format!("joint:{joint}"),
                            TrackTarget::BlendShape { channel } => format!("blend:{channel}"),
                        })
                        .collect();
                    eprintln!(
                        "[idtx-usd] full-path prim {} ({}) tracks=[{}]",
                        node.path,
                        node.type_name,
                        attrs.join(", ")
                    );
                }
            }
            let projection = self.service.sample_viewport_roots(tc, &roots)?;
            self.hydrator.apply_viewport_projection(&projection)?;
            self.snapshot_dirty = true;
            self.snapshot_transform_patches = None;
        } else if !skel_pose_ids.is_empty() {
            // Skeleton-pose fast path: only the joint palette changes, so
            // resample skeleton poses (poses-only) without re-projecting the
            // static mesh geometry/materials, then patch the GPU joint matrices
            // in place at render time — no whole-stage re-extract and no
            // `snapshot.serial` bump (which would rebuild the stereo eye0
            // buffers). Any concurrent xform-only tracks ride the transform
            // patch. Falls back to a full extract only if no snapshot exists yet.
            let projection = self.service.sample_skeleton_poses(tc, &skel_pose_ids)?;
            self.hydrator.apply_skeleton_poses(&projection)?;
            if self.last_snapshot.is_some() {
                self.snapshot_skeleton_dirty = true;
                self.snapshot_transform_patches = xform_patches;
            } else {
                self.snapshot_dirty = true;
                self.snapshot_transform_patches = None;
            }
        } else {
            // Pure xform-only animation: patch the existing snapshot in-place
            // instead of triggering a full O(N-meshes) re-extraction.
            self.snapshot_transform_patches = xform_patches;
        }

        #[cfg(feature = "metrics")]
        {
            self.last_anim_sample_ns = t_anim_sample.elapsed().as_nanos() as u64;
        }

        // Mark the clock as sampled.
        if let Some(mut clock) = self
            .hydrator
            .world_mut()
            .get_mut::<idtx_scene::StagePlayback>(root)
        {
            clock.mark_sampled();
        }

        Ok(())
    }

    /// Apply a UI-emitted playback command to the active stage's clock.
    /// Returns `true` when a clock exists and was updated.
    pub fn apply_playback_request(&mut self, req: idtx_scene::PlaybackRequest) -> bool {
        let Some(root) = self.pseudo_root else {
            return false;
        };
        idtx_scene::apply_playback_request(self.hydrator.world_mut(), root, req)
    }

    /// Advance every per-entity [`AnimClipPlayer`] by `dt_seconds` and resample
    /// the target subtrees of any playhead whose time code moved this frame.
    ///
    /// Distinct from [`Self::tick_animation`]'s stage-global `StagePlayback`
    /// pass: each playhead carries its own `targets` list and time code, so a
    /// resample is a `sample_viewport_roots(time_code, &targets)` call per
    /// playhead (the same bulk projection path the stage clock uses for its
    /// `full`/`skel` prims). A no-op — a single empty ECS query — when no entity
    /// carries a clip player, so it is cheap to call unconditionally each frame.
    fn tick_clip_animation(&mut self, dt_seconds: f64) -> Result<(), SessionError> {
        let any_pending = idtx_scene::tick_clip_players(self.hydrator.world_mut(), dt_seconds);
        if !any_pending {
            return Ok(());
        }

        let requests = idtx_scene::collect_clip_sample_requests(self.hydrator.world_mut());
        let mut sampled_any = false;
        for req in requests {
            let projection = self
                .service
                .sample_viewport_roots(req.time_code, &req.targets)?;
            self.hydrator.apply_viewport_projection(&projection)?;
            sampled_any = true;
        }
        if sampled_any {
            self.snapshot_dirty = true;
            self.snapshot_transform_patches = None;
        }

        // Clear the per-playhead sample-pending flags (including target-less or
        // range-less heads that produced no request) so the next frame only
        // resamples playheads whose time code actually changed.
        idtx_scene::mark_clip_players_sampled(self.hydrator.world_mut());
        Ok(())
    }

    /// Returns a snapshot of the current animation clip and clock state.
    ///
    /// `clip` is `None` when the stage has no animation. `clock` is `None`
    /// when the stage has animation metadata but no playable range.
    #[must_use]
    pub fn animation_state(
        &self,
    ) -> (
        Option<idtx_scene::StageAnimClip>,
        Option<idtx_scene::StagePlayback>,
    ) {
        let Some(root) = self.pseudo_root else {
            return (None, None);
        };
        let clip = self
            .hydrator
            .world()
            .get::<idtx_scene::StageAnimClip>(root)
            .cloned();
        let clock = self
            .hydrator
            .world()
            .get::<idtx_scene::StagePlayback>(root)
            .copied();
        (clip, clock)
    }

    /// Closes the session.  The renderer and GPU resources are freed when this
    /// struct is dropped; calling this explicitly is optional.
    ///
    /// All behavior teardown (the stage-unload broadcast + every instance's
    /// terminal lifecycle and guest destructor) runs in [`Drop`], so it happens
    /// on *every* ownership path — a shell tab close/reload (`tabs.remove`), a
    /// runtime suspend/exit, a bare drop, as well as this explicit `close`.
    /// This method just drops `self`, kept for call-site clarity / symmetry.
    pub fn close(self) {
        // Teardown is in `Drop`; dropping `self` here triggers it.
    }
}

impl Drop for StudioSession {
    /// Guarantees behavior teardown on *every* ownership path. The shell drops
    /// a tab's session via `tabs.remove(idx)` and the runtime suspend/exit path
    /// via `self.live.take()` — neither consumes [`Self::close`] — so relying on
    /// an explicit `close` would silently skip terminal hooks and guest
    /// destructors. Doing it here (where `&mut self` still owns a live `World`)
    /// closes that gap; the behavior host's own `Drop` cannot, as the hooks need
    /// `&mut World`.
    fn drop(&mut self) {
        #[cfg(feature = "metrics")]
        self.xr_metrics.write_json(self.source_path().file_stem().and_then(|s| s.to_str()).unwrap_or("session"));
        // Last-chance stage-unload signal to stage-scoped behaviors,
        // then the full terminal lifecycle (`on_stop?` → `on_detach` →
        // `on_destroy`) + guest destructor for every instance. `detach_all`
        // drains the host, so this is idempotent even after an explicit path
        // already ran it.
        self.dispatch_stage_event(StageEvent::Unloading);
        self.behaviors.detach_all(self.hydrator.world_mut());

        // Release the per-stage OpenExec bridge. The C++ `g_bridges` registry
        // owns both the `ExecBridge` and a strong `UsdStageRefPtr`, and only ever
        // shrinks on a topology rebuild (`tick_exec`); without this a normal
        // session drop would leave a permanent registry entry pinning the stage
        // and its OpenExec objects alive. Drop the subsystem first so its REST
        // pollers are signalled to stop before the bridge they feed goes away.
        #[cfg(feature = "openexec")]
        {
            self.exec = None;
            let _ = self.service.exec_release();
        }
    }
}

// ── OpenXR preview + behavior-host control ──────────────────────────────────────
//
// This impl block is unconditional (the behavior host is always compiled in).
// The OpenXR-specific methods are individually gated behind `xr-preview`.

/// The lifted `OpenXR` session, owned by the **runtime** between frames rather
/// than by a [`StudioSession`].
///
/// Level transitions drop and replace the active `StudioSession`
/// (`SessionSet::set_current`). If the live `OpenXR` session were owned *inside*
/// the session, that drop would run `XrPreviewSession::drop → end()` and tear
/// the headset down, forcing in-XR transitions to be refused. Owning the
/// session here lets the runtime keep it across a swap: it is
/// [installed](StudioSession::install_xr_runtime) into the active session only
/// while that session ticks/renders, and [taken back](StudioSession::take_xr_runtime)
/// before a swap, so the dropped session never owns XR.
///
/// `preview` carries the `OpenXR` session, the platform tracker, and the
/// per-world controller proxies; `gpu` (real-XR builds) carries the swapchain
/// wgpu wrappers, valid across a swap because every session shares one
/// `wgpu::Device`. The proxies are re-seeded into the new session's world on
/// install when the active session changed.
#[cfg(feature = "xr-preview")]
pub struct XrRuntimeSession {
    preview: xr_preview::XrPreviewSession,
    #[cfg(idtx_xr)]
    gpu: xr_render::XrGpu,
}

impl StudioSession {
    /// Which hands have their grip pose driven by an authored
    /// `*_controller_sync` behavior on the real controller prim (so the
    /// vestigial bridge proxy is suppressed for that hand). `[left, right]`.
    /// Shared by [`Self::start_xr_preview`] and the swap-time controller
    /// re-seed ([`xr_preview::XrPreviewSession::reseed_controllers`]).
    #[cfg(feature = "xr-preview")]
    fn controller_behavior_owned(&self) -> [bool; 2] {
        #[cfg(feature = "authoring")]
        {
            [
                self.xr_pose_prim_behavior_owned(xr_controller_prefab::LEFT_CONTROLLER_PATH),
                self.xr_pose_prim_behavior_owned(xr_controller_prefab::RIGHT_CONTROLLER_PATH),
            ]
        }
        #[cfg(not(feature = "authoring"))]
        {
            [false, false]
        }
    }

    /// Move the live `OpenXR` session **out** of this `StudioSession` without
    /// ending it, so the runtime can own it across a level swap. Returns `None`
    /// when no preview is active (desktop / before start), or — on a real-XR
    /// build — when the preview is present but its graphics session has not yet
    /// been bound (the preview is left in place in that case). The counterpart
    /// of [`Self::install_xr_runtime`]; see [`XrRuntimeSession`].
    #[cfg(feature = "xr-preview")]
    #[must_use]
    pub fn take_xr_runtime(&mut self) -> Option<XrRuntimeSession> {
        let preview = self.xr_preview.take()?;
        #[cfg(idtx_xr)]
        {
            let Some(gpu) = self.xr_gpu.take() else {
                // Preview present but graphics session not yet bound — put it
                // back so the caller's state is unchanged.
                self.xr_preview = Some(preview);
                return None;
            };
            Some(XrRuntimeSession { preview, gpu })
        }
        #[cfg(not(idtx_xr))]
        {
            Some(XrRuntimeSession { preview })
        }
    }

    /// Install a runtime-owned `OpenXR` session into this `StudioSession` so its
    /// tick / `render_xr` path can drive the headset. When `reseed` is set (the
    /// active session changed since the last install — a transfer cut or a
    /// level swap), the controller proxy entities are respawned in this
    /// session's ECS world. The counterpart of [`Self::take_xr_runtime`].
    #[cfg(feature = "xr-preview")]
    pub fn install_xr_runtime(&mut self, xr: XrRuntimeSession, reseed: bool) {
        let mut xr = xr;
        if reseed {
            let owned = self.controller_behavior_owned();
            xr.preview.reseed_controllers(self.hydrator.world_mut(), owned);
            // The respawned proxies must reach the next extract.
            self.snapshot_dirty = true;
        }
        self.xr_preview = Some(xr.preview);
        #[cfg(idtx_xr)]
        {
            self.xr_gpu = Some(xr.gpu);
        }
    }
}

impl StudioSession {
    /// Starts an `OpenXR` preview: probes the device and drives the XR
    /// lifecycle to focused, then spawns the fallback controller proxies for
    /// any hand whose pose is not already owned by an authored behavior.
    ///
    /// The camera is **not** owned here. The HMD-sync behavior is authored on
    /// `/World/Player/Camera` (attached via
    /// [`Self::attach_authored_behaviors`]) and drives that prim's
    /// `WorldTransform` through the session layer; the desktop mirror follows
    /// it through the normal scene-camera path
    /// ([`Self::use_scene_camera`] + [`Self::scene_camera_desc`]).
    ///
    /// # Errors
    ///
    /// Propagates [`xr_preview::XrPreviewError`] when no device is present
    /// or the lifecycle transition fails.
    #[cfg(feature = "xr-preview")]
    pub fn start_xr_preview(
        &mut self,
        loader: &idtx_openxr_sys::XrLoader,
    ) -> Result<(), xr_preview::XrPreviewError> {
        let session = {
            // Single-authority: if the loaded stage carries authored
            // `*_controller_sync` behaviors on the real controller prims, they
            // own the grip pose — suppress the synthetic bridge entities so the
            // pose is driven from exactly one place. Computed before the split-borrow.
            let controller_behavior_owned = self.controller_behavior_owned();

            // Split-borrow the hydrator: the XR preview spawns its fallback
            // controller proxies in the session's ECS world.
            let Self { hydrator, .. } = self;
            xr_preview::XrPreviewSession::start(
                loader,
                hydrator.world_mut(),
                controller_behavior_owned,
            )?
        };
        self.xr_preview = Some(session);
        // Legacy bake (`xr-bake`, off by default): geometry runs
        // eagerly here — ECS is already hydrated with the assembled XR stage.
        // Materials run lazily from inject_xr_controller_meshes()
        // once the render snapshot has resolved the XR assembly's material map
        // (snapshot.serial gates it so it only runs when the snapshot actually
        // changes). In a default build the controllers hydrate as visible
        // prefabs, so no mesh is sourced or baked here.
        #[cfg(feature = "xr-bake")]
        {
            let mut mesh_cache = xr_controller_mesh::ControllerMeshCache::new();
            mesh_cache.source_geometry(&self.hydrator);
            self.xr_controller_mesh = Some(mesh_cache);
        }
        self.snapshot_dirty = true;
        Ok(())
    }

    /// Create a real graphics-bound `OpenXR` session from the
    /// persistent instance + the wgpu D3D12 device/queue handles, and attach
    /// it to the active preview. Confines the `unsafe` binding call here so
    /// the shell can stay `#![forbid(unsafe_code)]`. No-op if no preview.
    ///
    /// # Errors
    ///
    /// Propagates [`xr_preview::XrPreviewError`] if the graphics binding fails.
    #[cfg(all(idtx_xr, windows))]
    pub fn start_xr_graphics_session(
        &mut self,
        instance: &mut idtx_openxr_sys::XrInstance,
        device: *mut std::ffi::c_void,
        queue: *mut std::ffi::c_void,
    ) -> Result<(), xr_preview::XrPreviewError> {
        // SAFETY: the caller (shell) passes wgpu's live D3D12 device+queue,
        // which outlive the session, on the adapter the instance's LUID pinned.
        let mut session = unsafe { instance.create_session(device, queue) }?;

        // Probe hand-tracking support and wire up the hand trackers.
        // Non-fatal — a device without XR_EXT_hand_tracking simply falls back
        // to controller capsule rendering.
        #[cfg(feature = "hand-tracking")]
        {
            let supported = instance.try_init_hand_tracking(&mut session);
            if let Some(xr) = self.xr_preview.as_mut() {
                if supported {
                    xr.platform_mut().mark_hand_tracking_supported();
                }
            }
        }

        // Build the Stage C stereo render resources from the fresh session
        // (swapchain images wrapped as wgpu textures + per-eye targets).
        self.xr_gpu = Some(xr_render::XrGpu::new(&self.renderer, &session)?);
        if let Some(xr) = self.xr_preview.as_mut() {
            xr.set_graphics_session(session);
        }
        Ok(())
    }

    /// Create a real graphics-bound `OpenXR` session (Quest/Android) from the
    /// persistent instance + the raw Vulkan handles backing the wgpu device
    /// (`VkInstance` / `VkPhysicalDevice` / `VkDevice` + queue family/index),
    /// and attach it to the active preview. The Vulkan parallel to
    /// [`Self::start_xr_graphics_session`]; confines the `unsafe` binding call
    /// here so the shell can stay `#![forbid(unsafe_code)]`. No-op if no preview.
    ///
    /// Hand tracking is intentionally not initialised on Quest (deferred for
    /// frame-rate reasons — controllers only).
    ///
    /// # Errors
    ///
    /// Propagates [`xr_preview::XrPreviewError`] if the graphics binding fails.
    #[cfg(all(idtx_xr, target_os = "android"))]
    pub fn start_xr_graphics_session_vk(
        &mut self,
        instance: &mut idtx_openxr_sys::XrInstance,
        vk_instance: *const std::ffi::c_void,
        physical_device: *const std::ffi::c_void,
        vk_device: *const std::ffi::c_void,
        queue_family_index: u32,
        queue_index: u32,
    ) -> Result<(), xr_preview::XrPreviewError> {
        // SAFETY: the caller (runtime player) passes the live Vulkan handles
        // backing wgpu's device/queue, which outlive the session, on the
        // physical device the runtime drives the headset with.
        let session = unsafe {
            instance.create_session_vk(
                vk_instance,
                physical_device,
                vk_device,
                queue_family_index,
                queue_index,
            )
        }?;

        // Build the Stage C stereo render resources from the fresh session
        // (swapchain images wrapped as wgpu textures + per-eye targets).
        self.xr_gpu = Some(xr_render::XrGpu::new(&self.renderer, &session)?);
        if let Some(xr) = self.xr_preview.as_mut() {
            xr.set_graphics_session(session);
        }
        Ok(())
    }

    /// Render one stereo frame to the headset (Stage C): begin the XR frame,
    /// render both eyes into the swapchain images, submit the projection layer,
    /// and stash the left-eye camera for the desktop mirror.
    ///
    /// Returns `None` while the preview should keep running, or `Some(message)`
    /// when the runtime ended the session (headset removed, exited via the
    /// system menu, session lost, or a frame error) — the caller should then
    /// call [`Self::stop_xr_preview`] and surface `message`.
    #[cfg(idtx_xr)]
    #[must_use]
    pub fn render_xr(&mut self, wireframe: bool) -> Option<String> {
        const NEAR: f32 = 0.05;
        const FAR: f32 = 5000.0;

        let frame = match self.xr_preview.as_mut() {
            Some(xr) => match xr.begin_xr_frame() {
                xr_preview::XrBeginOutcome::Frame(f) => f,
                xr_preview::XrBeginOutcome::Skip => return None,
                xr_preview::XrBeginOutcome::Exit(msg) => {
                    return Some(msg.unwrap_or_else(|| "OpenXR session ended.".to_owned()));
                }
            },
            None => return Some("OpenXR session ended.".to_owned()),
        };

        #[cfg(feature = "metrics")]
        let t_total = std::time::Instant::now();

        // Author the tracked pose of any hydrated controller onto its
        // USD pose Xform and reproject, so the extract below picks it up.
        #[cfg(all(feature = "xr-preview", feature = "authoring"))]
        {
            #[cfg(feature = "metrics")]
            let t_author = std::time::Instant::now();
            self.author_hydrated_controller_poses();
            #[cfg(feature = "metrics")]
            {
                self.xr_last_author_ns = t_author.elapsed().as_nanos() as u64;
            }

            // One drain + reprojection per XR frame: picks up BOTH the controller/
            // pointer poses authored just above AND the dynamic physics poses
            // `tick_physics` authored earlier this frame with its own drain deferred
            // (`defer_reprojection = true`) — previously each ran its own full
            // drain+reproject pass, two per frame. Cheap when nothing changed, as
            // `drain_changes` early-outs. Kept before the visibility pass below so
            // visibility `over`s are still authored after the reprojection, as
            // before this moved out of `author_hydrated_controller_poses`.
            #[cfg(feature = "metrics")]
            let t_reproject = std::time::Instant::now();
            if let Err(e) = self.tick() {
                tracing::warn!("reproject XR authored poses: {e}");
            }
            #[cfg(feature = "metrics")]
            {
                self.xr_last_reproject_ns = t_reproject.elapsed().as_nanos() as u64;
            }

            // Hide each controller + pointer whose hand is actively hand-tracked
            // (per the current visualisation mode) so only the hand skeleton draws
            // for it. Sets `snapshot_dirty` on a change, so the extract below picks
            // it up; a no-op in steady state.
            self.apply_xr_hand_controller_visibility();
        }

        // Ensure the scene snapshot exists (reuse the cached one otherwise).
        #[cfg(feature = "metrics")]
        let t_extract = std::time::Instant::now();
        let _maintenance = self.prepare_presentation(1.0);
        #[cfg(feature = "metrics")]
        let extracted_this_frame = _maintenance.extracted;
        #[cfg(feature = "metrics")]
        let extract_ns = t_extract.elapsed().as_nanos() as u64;

        // Track the controllers in the headset views too: inject their capsule
        // meshes before rendering the eyes (previously this happened only in the
        // desktop `render_one_frame`, which no longer runs during the pcvr XR
        // preview — see `render_xr`'s mirror blit below). Legacy bake only
        // (`xr-bake`); hydrated controllers render from the snapshot.
        #[cfg(feature = "xr-bake")]
        self.inject_xr_controller_meshes();
        #[cfg(feature = "metrics")]
        let t_hand = std::time::Instant::now();
        self.inject_xr_hand_meshes();
        #[cfg(feature = "metrics")]
        let hand_inject_ns = t_hand.elapsed().as_nanos() as u64;
        #[cfg(feature = "metrics")]
        let hand_instance_count = self.last_snapshot.as_ref().map_or(0, |s| {
            xr_hand_mesh::count_hand_batch_instances(&s.instanced_batches) as u32
        });

        // Place the OpenXR play space into the virtual world for the eye cameras:
        // the player pawn's world matrix (locomotion + snap-turn yaw authored on
        // /World/Player by `xr_player_move`), composed with `projection_basis`.
        // Computed here — after extract/reproject, before the `xr_gpu`/`renderer`
        // borrows — so it reflects this frame's authored transform with no added
        // latency, and so the method call does not overlap those borrows. Identity
        // when no local-player pawn resolves (desktop rig / none), leaving the eye
        // at its raw located pose (the pre-pawn behaviour). The hydrated children
        // of /World/Player already follow this matrix via USD composition; this is
        // the one place the rendered eyes were missing it.
        let stage_to_world = self.local_player_root_matrix().unwrap_or(glam::Mat4::IDENTITY);

        let Some(xr_gpu) = self.xr_gpu.as_ref() else {
            if let Some(xr) = self.xr_preview.as_mut() {
                let _ = xr.submit_xr_frame(false, None);
            }
            return None;
        };
        let snapshot = self.last_snapshot.as_mut().unwrap();
        let eye_timings = xr_gpu.render_eyes(
            &mut self.renderer,
            &frame,
            snapshot,
            stage_to_world,
            self.xr_recenter,
            &self.presentation_overlay_lines,
            wireframe,
            NEAR,
            FAR,
        );
        // Chain a depth composition layer only when the eyes actually produced
        // and copied depth into the depth swapchain this frame.
        let depth_range = if xr_gpu.has_depth_layer() {
            Some((NEAR, FAR))
        } else {
            None
        };
        #[cfg(not(feature = "metrics"))]
        let _ = eye_timings;

        // Mirror the left eye into the desktop preview target with a cheap
        // texture blit, so the scene is rendered twice (one per eye) rather than
        // a third time for the mirror. The shell skips `render_one_frame` on
        // frames this path produced.
        self.renderer
            .blit_into(xr_gpu.mirror_source(0), &self.frame_target);

        // Push the located controller poses into the platform so the next
        // `tick_behaviors` call feeds them into the behavior host snapshot.
        if let Some(xr) = self.xr_preview.as_mut() {
            use idtx_platform_xr::ControllerPose;
            let to_ctrl = |lp: idtx_openxr_sys::LocatedPose| ControllerPose {
                position: glam::Vec3::from(lp.position),
                orientation: glam::Quat::from_array(lp.orientation),
                timestamp: 0.0,
            };
            xr.platform_mut().set_controller_poses(
                frame.left_controller.map(to_ctrl),
                frame.right_controller.map(to_ctrl),
            );

            // Aim poses drive the pointer rays (distinct from the grip poses
            // above). Same STAGE space, ergonomically-tilted "points-at" pose.
            xr.platform_mut()
                .set_aim_poses(frame.left_aim.map(to_ctrl), frame.right_aim.map(to_ctrl));

            // Push controller button / axis state (thumbsticks, triggers, grips,
            // face buttons) so the next `tick_behaviors` feeds it into the input
            // router as `Controller/MetaQuest/*` sources.
            xr.platform_mut().set_controller_input(
                controller_buttons(&frame.left_input),
                controller_buttons(&frame.right_input),
            );

            // Push the HMD (head) pose, derived from the two located eye poses,
            // so the next `tick_behaviors` feeds a *tracked* head into the host
            // snapshot. Without this `XrPlatform::current_pose()` is `None` on the
            // real pcvr path (`HmdTracker::poll()` is a no-op stub and nothing
            // else calls `set_current_pose`), so `feed_xr_snapshot` leaves the
            // `Hmd` channel untracked — and gaze-relative behaviors that gate on
            // `get_pose(XrSpace::Hmd)` (`xr_player_move` locomotion,
            // `hmd_camera_sync`) early-return every frame, so the left thumbstick
            // never moves the pawn. Same one-frame latency as the controller feed
            // above (harmless; already present there).
            xr.platform_mut()
                .set_current_pose(head_pose_from_eyes(frame.eyes[0].pose, frame.eyes[1].pose));

            // Push hand joint poses for the hand skeleton renderer.
            #[cfg(feature = "hand-tracking")]
            {
                use idtx_platform_xr::{HandJoint, HandPose};
                let to_hand = |joints: Box<[idtx_openxr_sys::LocatedHandJoint; 26]>| {
                    let mut pose = HandPose::zeroed();
                    for (i, j) in joints.iter().enumerate() {
                        pose.joints[i] = HandJoint {
                            position: glam::Vec3::from(j.position),
                            orientation: glam::Quat::from_array(j.orientation),
                            radius: j.radius,
                        };
                    }
                    pose
                };
                xr.platform_mut()
                    .set_hand_poses(frame.left_hand.map(to_hand), frame.right_hand.map(to_hand));
            }
        }

        #[cfg(feature = "metrics")]
        let t_submit = std::time::Instant::now();
        let submit_result = match self.xr_preview.as_mut() {
            Some(xr) => xr.submit_xr_frame(true, depth_range).err(),
            None => None,
        };

        #[cfg(feature = "metrics")]
        {
            let submit_ns = t_submit.elapsed().as_nanos() as u64;
            let pacing = self
                .xr_preview
                .as_ref()
                .and_then(|xr| xr.graphics_session())
                .map(|gs| gs.pacing_metrics());
            let mut timings = xr_metrics::XrFrameTimings {
                total_ns: t_total.elapsed().as_nanos() as u64,
                author_poses_ns: self.xr_last_author_ns,
                tick_reproject_ns: self.xr_last_reproject_ns,
                extract_ns,
                extracted_this_frame,
                hand_inject_ns,
                hand_instance_count,
                eye_render_ns: [eye_timings.frames[0].total_ns, eye_timings.frames[1].total_ns],
                eye_mesh_count: [
                    eye_timings.frames[0].mesh_count,
                    eye_timings.frames[1].mesh_count,
                ],
                eye_instance_count: [
                    eye_timings.frames[0].instance_count,
                    eye_timings.frames[1].instance_count,
                ],
                eye_cache_misses: [
                    eye_timings.frames[0].cache_misses,
                    eye_timings.frames[1].cache_misses,
                ],
                eye_evictions: [
                    eye_timings.frames[0].evictions,
                    eye_timings.frames[1].evictions,
                ],
                eye_draw_list_ns: [
                    eye_timings.frames[0].draw_list_build_ns,
                    eye_timings.frames[1].draw_list_build_ns,
                ],
                eye_gpu_passes_ns: [
                    eye_timings.frames[0].gpu_passes_ns,
                    eye_timings.frames[1].gpu_passes_ns,
                ],
                // GPU-execution timings propagated from the per-eye
                // RenderFrameTimings (Gap B) — real on-GPU time, 0 unless the
                // adapter supports timestamp queries (Gap A).
                eye_gpu_pbr_ns: [
                    eye_timings.frames[0].gpu_pbr_ns,
                    eye_timings.frames[1].gpu_pbr_ns,
                ],
                eye_gpu_sample_id: [eye_timings.frames[0].gpu_sample_id, eye_timings.frames[1].gpu_sample_id],
                eye_viewport: [eye_timings.frames[0].viewport, eye_timings.frames[1].viewport],
                eye_gpu_tonemap_ns: [
                    eye_timings.frames[0].gpu_tonemap_ns,
                    eye_timings.frames[1].gpu_tonemap_ns,
                ],
                eye_gpu_shadow_ns: [
                    eye_timings.frames[0].gpu_shadow_ns,
                    eye_timings.frames[1].gpu_shadow_ns,
                ],
                eye_overdraw: [
                    eye_timings.frames[0].fragment_shader_invocations,
                    eye_timings.frames[1].fragment_shader_invocations,
                ],
                eye_gpu_wait_ns: [
                    eye_timings.frames[0].gpu_wait_ns,
                    eye_timings.frames[1].gpu_wait_ns,
                ],
                // Swapchain copy + depth copy timings (Gap C) measured in
                // render_eyes, outside the renderer's own instrumentation.
                eye_swapchain_copy_ns: eye_timings.swapchain_copy_ns,
                eye_depth_copy_ns: eye_timings.depth_copy_ns,
                // Draw-call / state-change counters — the CPU-bound-submission
                // signals (shared with the visionOS feeder).
                eye_draw_calls: [
                    eye_timings.frames[0].draw_calls,
                    eye_timings.frames[1].draw_calls,
                ],
                eye_pipeline_switches: [
                    eye_timings.frames[0].pipeline_switches,
                    eye_timings.frames[1].pipeline_switches,
                ],
                eye_bind_group_sets: [
                    eye_timings.frames[0].bind_group_sets,
                    eye_timings.frames[1].bind_group_sets,
                ],
                eye_merged_batch_count: [
                    eye_timings.frames[0].merged_batch_count,
                    eye_timings.frames[1].merged_batch_count,
                ],
                submit_ns,
                ..Default::default()
            };
            if let Some(p) = pacing {
                timings.wait_frame_ns = p.wait_frame_ns;
                timings.acquire_wait_ns = p.acquire_wait_ns;
                timings.frame_interval_ns = p.frame_interval_ns;
                timings.empty_frame_count = p.empty_frame_count;
                timings.depth_layer_submitted = p.depth_layer_submitted;
            }
            self.xr_metrics.record(timings);
            if self.xr_metrics.should_log() {
                tracing::info!("{}", self.xr_metrics.log_line());
            }
        }

        submit_result
    }

    /// Stops the active `OpenXR` preview (if any), returning the viewport to
    /// the orbit camera. Idempotent.
    #[cfg(feature = "xr-preview")]
    pub fn stop_xr_preview(&mut self) {
        // Dump the rolling XR-frame metrics before tearing the session down.
        #[cfg(feature = "metrics")]
        {
            let stage_name = self
                .source_path
                .file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("stage")
                .to_owned();
            self.xr_metrics.write_json(&stage_name);
        }
        // Drop the wrapped swapchain textures before the session/swapchains.
        #[cfg(idtx_xr)]
        {
            self.xr_gpu = None;
        }
        if let Some(mut xr) = self.xr_preview.take() {
            xr.end();
        }
        // Restore any controller/pointer hidden by hand-tracking gating, so a
        // later session does not inherit invisible controllers.
        #[cfg(all(feature = "xr-preview", feature = "authoring"))]
        {
            for (idx, path) in [
                (0usize, xr_controller_prefab::LEFT_CONTROLLER_PATH),
                (1usize, xr_controller_prefab::RIGHT_CONTROLLER_PATH),
                (2usize, xr_controller_prefab::LEFT_POINTER_PATH),
                (3usize, xr_controller_prefab::RIGHT_POINTER_PATH),
            ] {
                if !self.xr_pose_prim_visible[idx] {
                    if let Err(e) = self.service.author_session_visibility(path, true) {
                        tracing::debug!("restore xr controller visibility {path}: {e}");
                    }
                    self.force_prim_visibility(path, true);
                    self.xr_pose_prim_visible[idx] = true;
                }
            }
        }
        self.snapshot_dirty = true;
    }

    /// `true` while an `OpenXR` preview is active.
    #[cfg(feature = "xr-preview")]
    #[must_use]
    pub fn xr_preview_active(&self) -> bool {
        self.xr_preview.is_some()
    }

    /// The current behavior transport state.
    #[must_use]
    pub fn behavior_play_state(&self) -> PlayState {
        self.play_state
    }

    /// Set the behavior transport state. A `Playing`→`Authoring` transition
    /// fires `on_stop`; the first `Playing` frame fires `on_start` (both
    /// handled by the host inside [`Self::tick_behaviors`]/`run_frame`).
    ///
    /// The coarse stage-level transport is also broadcast to stage-scoped
    /// behaviors: entering `Playing` dispatches
    /// [`StageEvent::PlayStart`]; leaving it dispatches
    /// [`StageEvent::PlayStop`]. A no-op transition (same state) broadcasts
    /// nothing.
    pub fn set_play_state(&mut self, play: PlayState) {
        let prev = self.play_state;
        self.play_state = play;
        match (prev, play) {
            (PlayState::Authoring, PlayState::Playing) => {
                self.dispatch_stage_event(StageEvent::PlayStart);
            }
            (PlayState::Playing, PlayState::Authoring) => {
                self.dispatch_stage_event(StageEvent::PlayStop);
                self.clear_runtime_spawns();
            }
            _ => {}
        }
    }

    /// `true` when the behavior transport is `Playing`.
    #[must_use]
    pub fn is_behavior_playing(&self) -> bool {
        matches!(self.play_state, PlayState::Playing)
    }

    /// Convenience toggle so a caller need not import [`PlayState`].
    pub fn set_behavior_playing(&mut self, playing: bool) {
        // Route through `set_play_state` so the stage-event broadcast fires.
        self.set_play_state(if playing {
            PlayState::Playing
        } else {
            PlayState::Authoring
        });
    }

    /// Replace the behavior host with a fresh, empty one built with `limits`.
    /// Must be called *before* any attach (it discards all attached instances).
    /// Lets an embedder (e.g. the runtime player) opt into [`Limits::runtime`]
    /// without changing `open_with_gpu`'s signature.
    ///
    /// Any instances already attached are torn down through their full terminal
    /// lifecycle (+ guest destructors) via [`BehaviorHost::detach_all`] before
    /// the host is dropped, so replacing the host never silently drops a live
    /// guest store.
    pub fn set_behavior_limits(&mut self, limits: Limits) {
        self.clear_runtime_spawns();
        let state = self.behaviors.experience_state();
        self.behaviors.detach_all(self.hydrator.world_mut());
        self.behaviors = BehaviorHost::new(limits, InputRouter::default());
        self.behaviors.set_experience_state(state);
        self.behavior_reconcile_pending = self.authored_behaviors_enabled;
        self.install_scene_backend();
    }

    pub fn experience_state(&self) -> idtx_behavior::ExperienceState { self.behaviors.experience_state() }

    pub fn set_experience_state(&mut self, state: idtx_behavior::ExperienceState) {
        self.behaviors.set_experience_state(state);
    }

    /// (Re)install the scene-query backend (raycast / arc-cast / prim-type
    /// query) on the behavior host for the current projection basis. Called at
    /// construction, whenever the basis changes (units / up-axis), and whenever
    /// the host is rebuilt.
    fn install_scene_backend(&mut self) {
        self.behaviors.set_spawn_generation(self.runtime_spawn_generation());
        self.behaviors.set_auth_scopes(self.auth_scopes.clone());
        self.behaviors.set_scene_backend(std::sync::Arc::new(
            scene_backend_impl::StudioSceneBackend::new(
                self.projection_basis,
                self.projection_basis_inv,
            ),
        ));
        // Re-arm the behavior `net` SSRF policy whenever the host is (re)built —
        // the fresh `BehaviorHost` otherwise resets it to deny-by-default.
        #[cfg(feature = "remote")]
        self.behaviors
            .set_network_policy(self.exec_network_policy.clone());
    }

    /// Install the scene-query backend on an **externally-owned** behavior host.
    ///
    /// The standalone visionOS runtime drives its own [`BehaviorHost`] and never
    /// calls [`tick_behaviors`](Self::tick_behaviors) (which installs the backend
    /// on the session's private host), so without this its `raycast` /
    /// `collision_body` / `physics_body` imports return `NotFound` and any gaze /
    /// aim behavior silently no-ops. Mirrors [`install_scene_backend`] against a
    /// caller-provided host for the current projection basis.
    pub fn install_scene_backend_on(&self, host: &mut idtx_behavior::BehaviorHost) {
        host.set_auth_scopes(self.auth_scopes.clone());
        host.set_scene_backend(std::sync::Arc::new(
            scene_backend_impl::StudioSceneBackend::new(
                self.projection_basis,
                self.projection_basis_inv,
            ),
        ));
        // Mirror the behavior `net` SSRF policy onto the externally-owned host so
        // a visionOS-runtime behavior enforces the same allowlist.
        #[cfg(feature = "remote")]
        host.set_network_policy(self.exec_network_policy.clone());
    }

    /// Resolve overlay-highlight commands drained from an **externally-owned**
    /// behavior host into `self.behavior_highlights`, so the visionOS eye path can
    /// inject them onto the render snapshot. The standalone runtime drains its own
    /// host's overlay queue and feeds it here, parity with the hydrator-side
    /// resolve the `OpenXR`/desktop `tick_behaviors` does inline. `widget_markers`
    /// are the host's drained [`idtx_behavior::WidgetMarker`]s for this frame —
    /// lowered into the same `behavior_overlay_lines` as hit-point cue rings, the
    /// piece the inline `run_frame` path adds that the external path previously
    /// dropped. Returns the number of highlights that resolved to a prim (for
    /// runtime diagnostics).
    pub fn resolve_overlay_highlights_external(
        &mut self,
        commands: &[idtx_behavior::OverlayCommand],
        widget_markers: &[idtx_behavior::WidgetMarker],
    ) -> usize {
        self.behavior_highlights = resolve_overlay_highlights(&self.hydrator, commands);
        // Teleport arc + marker lines travel the same channel; resolve them
        // alongside the ABI-3 widget hit-point cue rings via the shared helper so
        // the visionOS eye path draws exactly what the inline `run_frame` path
        // does — previously only the overlay commands were lowered here, so the
        // gaze/pinch cue rendered on desktop but never on the headset.
        self.behavior_overlay_lines = assemble_behavior_overlay_lines(
            commands,
            widget_markers,
            self.world_unit_scale.metres_per_unit(),
        );
        self.behavior_highlights.len()
    }

    /// On-device collision-hydration diagnostic (visionOS has no debugger):
    /// `(enabled colliders, total CollisionComp, ViewportPickable+BoundsComp
    /// entities)`. `enabled == 0` means collision never hydrated (schema not
    /// registered / payload unloaded) so collidable-only raycasts can never hit;
    /// `pickable == 0` means the broad phase has no candidates at all.
    pub fn collision_debug_counts(&mut self) -> (usize, usize, usize) {
        use idtx_scene::bevy_ecs::prelude::With;
        use idtx_scene::{BoundsComp, CollisionComp, ViewportPickable};
        let world = self.hydrator.world_mut();
        let mut total = 0usize;
        let mut enabled = 0usize;
        let mut q = world.query::<&CollisionComp>();
        for c in q.iter(world) {
            total += 1;
            if c.enabled {
                enabled += 1;
            }
        }
        let mut pq =
            world.query_filtered::<(), (With<ViewportPickable>, With<BoundsComp>)>();
        let pickable = pq.iter(world).count();
        (enabled, total, pickable)
    }

    /// On-device gaze-raycast diagnostic: run the exact `interaction_only` pick a
    /// behavior would (stage-space origin/dir, require-collidable + Interaction
    /// channel, 50 m) via the real [`StudioSceneBackend`], returning the hit prim
    /// path + stage-space distance. Lets the runtime log whether the host pick
    /// finds a collider — isolating the picking/hydration layer from the WASM
    /// behavior's own pose read and overlay post.
    pub fn debug_gaze_raycast(
        &mut self,
        origin: glam::Vec3,
        dir: glam::Vec3,
    ) -> Option<(String, f32)> {
        use idtx_behavior::SceneBackend;
        let backend = scene_backend_impl::StudioSceneBackend::new(
            self.projection_basis,
            self.projection_basis_inv,
        );
        let params = idtx_behavior_abi::RaycastParams {
            origin: origin.into(),
            dir: dir.into(),
            type_filter_hash: idtx_behavior_abi::TYPE_HASH_ANY,
            max_distance: 50.0,
            flags: idtx_behavior_abi::RAYCAST_REQUIRE_COLLIDABLE
                | idtx_behavior_abi::RAYCAST_RESPECT_VISIBILITY,
            purpose_mask: 0,
            channel_mask: idtx_behavior_abi::CHANNEL_INTERACTION,
        };
        let hit = backend.raycast(self.hydrator.world_mut(), &params);
        if hit.hit == 0 {
            return None;
        }
        let entity =
            idtx_scene::bevy_ecs::entity::Entity::try_from_bits(hit.entity.to_bits())?;
        let path = self
            .hydrator
            .world()
            .get::<idtx_scene::PrimNode>(entity)
            .map(|n| n.path.clone())
            .unwrap_or_default();
        Some((path, hit.distance))
    }

    /// The active view/player camera as a behavior-facing [`CameraPose`] (stage
    /// space) **and** its full world transform: the scene/player camera when one
    /// is active, else the desktop orbit camera. Lets a behavior raycast from the
    /// viewpoint it is seeing and rigidly attach a grabbed prim to the camera
    /// frame. The transform is the camera's world matrix (`view^-1`); `IDENTITY`
    /// when no camera is available (paired with a `valid == 0` pose).
    /// The active view/player-camera pose (stage space) + its world transform,
    /// derived from the bound scene camera (or the orbit fallback). Seeded into
    /// the behavior host before each `run_frame` so a behavior can raycast from
    /// the viewpoint ([`BehaviorHost::set_camera_pose`]) and rigidly attach a
    /// grabbed prim to the camera frame ([`BehaviorHost::set_camera_transform`]).
    /// `tick_behaviors` seeds it internally; the visionOS runtime — which drives
    /// its own host — calls this and seeds the host itself for parity.
    pub fn view_camera_pose(&mut self) -> (idtx_behavior_abi::CameraPose, glam::Mat4) {
        let aspect = self.viewport_pixels[0] as f32 / self.viewport_pixels[1].max(1) as f32;
        let desc = self
            .scene_camera_desc(aspect)
            .unwrap_or_else(|| self.orbit_camera.to_camera_desc(aspect));
        // The view matrix is the inverse of the camera's world transform; invert
        // it back to recover eye / look / up in render(=stage) space. `inv` is
        // also the camera's world transform handed to behaviors verbatim.
        let inv = desc.view.inverse();
        let eye = inv.w_axis.truncate();
        let forward = (-inv.z_axis.truncate()).normalize_or_zero();
        let up = inv.y_axis.truncate().normalize_or_zero();
        if forward == Vec3::ZERO {
            return (idtx_behavior_abi::CameraPose::default(), glam::Mat4::IDENTITY);
        }
        let pose = idtx_behavior_abi::CameraPose {
            position: eye.into(),
            forward: forward.into(),
            up: up.into(),
            valid: 1,
        };
        (pose, inv)
    }

    /// Install the named-input mapping the behavior host resolves actions
    /// against. Rebuilds the host router from `map`; behaviors that
    /// already resolved action ids keep their ids stable because
    /// [`InputMap::parse`] assigns ids deterministically. Call before or after
    /// attach — resolution happens per attach and per hot read.
    pub fn set_behavior_input_map(&mut self, map: InputMap) {
        self.behaviors.set_router(InputRouter::new(map));
    }

    /// Borrow the host's input router — the map behaviors resolve
    /// action names against this frame. Additive read accessor, symmetric with
    /// [`Self::set_behavior_input_map`]; lets an embedder (or a test) inspect the
    /// currently-installed bindings, e.g. after a mapping-context reload.
    #[must_use]
    pub fn behavior_router(&self) -> &InputRouter {
        self.behaviors.router()
    }

    // ── input mapping-context stack (runtime hot-reload) ─────────────
    //
    // The stack layers named [`InputMap`]s by priority with consuming
    // shadowing (see [`MappingContextStack`]); every mutation re-flattens it
    // and reinstalls the effective map into the host router *immediately*, so
    // pushing a "menu" context on top of "gameplay" takes effect on the next
    // frame's `tick_behaviors` without any per-frame reload. The reload is
    // strictly user/runtime-driven — `tick_behaviors` never touches the stack.
    // Behavior execution is a runtime-only concern, so this surface is only
    // built for `xr-preview` embedders and is never wired to the editor shell.

    /// Push a named mapping context at `priority`, or replace an existing one of
    /// the same `name`, then hot-reload the effective bindings into the host
    /// router. Returns `true` when a same-name context was replaced.
    pub fn push_mapping_context(
        &mut self,
        name: impl Into<String>,
        priority: i32,
        map: InputMap,
    ) -> bool {
        let replaced = self.mapping_contexts.add(name, priority, map);
        self.reload_mapping_contexts();
        replaced
    }

    /// Remove the mapping context named `name` and hot-reload the effective
    /// bindings. Returns `true` when a context was removed (a no-op miss leaves
    /// the installed router untouched).
    pub fn remove_mapping_context(&mut self, name: &str) -> bool {
        let removed = self.mapping_contexts.remove(name);
        if removed {
            self.reload_mapping_contexts();
        }
        removed
    }

    /// Change the priority of the context named `name`, re-sorting the stack and
    /// hot-reloading the effective bindings. Returns `true` when the context
    /// existed (its map is preserved). No-op — including no reload — when no
    /// context of that name is present.
    pub fn reprioritise_mapping_context(&mut self, name: &str, priority: i32) -> bool {
        let Some(map) = self
            .mapping_contexts
            .contexts()
            .find(|c| c.name == name)
            .map(|c| c.map.clone())
        else {
            return false;
        };
        // `add` removes the existing same-name context first, then re-inserts at
        // the new priority and re-sorts — preserving the authored bindings.
        self.mapping_contexts.add(name, priority, map);
        self.reload_mapping_contexts();
        true
    }

    /// Drop every mapping context and hot-reload. No-op (no reload) when the
    /// stack is already empty, so a stray clear never clobbers a map installed
    /// directly via [`Self::set_behavior_input_map`].
    pub fn clear_mapping_contexts(&mut self) {
        if !self.mapping_contexts.is_empty() {
            self.mapping_contexts.clear();
            self.reload_mapping_contexts();
        }
    }

    /// Number of active mapping contexts.
    #[must_use]
    pub fn mapping_context_count(&self) -> usize {
        self.mapping_contexts.len()
    }

    /// The active mapping-context names, highest priority first.
    pub fn mapping_context_names(&self) -> impl Iterator<Item = &str> {
        self.mapping_contexts.contexts().map(|c| c.name.as_str())
    }

    /// Re-flatten the mapping-context stack and reinstall the effective map into
    /// the host router. Called after every stack mutation; splitting it out
    /// avoids a double borrow (compute the map into a local first, then hand it
    /// to [`Self::set_behavior_input_map`]).
    fn reload_mapping_contexts(&mut self) {
        let map = self.mapping_contexts.effective_map();
        self.set_behavior_input_map(map);
    }

    /// Report a digital input source as held this frame, e.g.
    /// `"Key/Space"` / `"Gamepad/South"`. Held sources persist across frames
    /// until [`Self::release_input`]; edges (Pressed/Held/Released) are derived
    /// by the host router from the previous frame's held set.
    pub fn press_input(&mut self, source: impl Into<String>) {
        self.frame_input.press(source);
    }

    /// Release a digital input source previously reported via
    /// [`Self::press_input`].
    pub fn release_input(&mut self, source: &str) {
        self.frame_input.release(source);
    }

    /// Set an analogue input source's value this frame, e.g.
    /// `"Gamepad/LeftStickX" → 0.6`. Persists until overwritten or cleared.
    pub fn set_analog_input(&mut self, source: impl Into<String>, value: f32) {
        self.frame_input.set_analog(source, value);
    }

    /// Set a 2D vector input source this frame, e.g. a thumbstick
    /// (`"Controller/MetaQuest/LeftThumbstick" → [0.4, -0.9]`). Consumed by
    /// `axis2` actions. Persists until overwritten or cleared.
    pub fn set_vector2_input(&mut self, source: impl Into<String>, value: [f32; 2]) {
        self.frame_input.set_vector2(source, value);
    }

    /// Clear a 2D vector input source (returns it to `[0, 0]`).
    pub fn clear_vector2_input(&mut self, source: &str) {
        self.frame_input.clear_vector2(source);
    }

    /// Accumulate a mouse delta for this frame; cleared after the tick.
    pub fn add_mouse_input(&mut self, delta: [f32; 2]) {
        self.frame_input.add_mouse_delta(delta);
    }

    /// Accumulate a scroll delta for this frame; cleared after the tick.
    pub fn add_scroll_input(&mut self, scroll: f32) {
        self.frame_input.add_scroll(scroll);
    }

    /// Build a [`BehaviorManifest`] from the `IdtxBehaviorAPI` schema attributes
    /// authored on the prim at `prim_path` — the single source of truth for a
    /// behavior's host manifest (no sidecar `idtx.toml`, no fallback). Returns
    /// `None` when the prim cannot be resolved. Exposed so out-of-crate attach
    /// loops (e.g. the packaged `idtx-runtime` player) share this one decoder.
    #[must_use]
    pub fn behavior_manifest_from_schema(&self, prim_path: &str) -> Option<BehaviorManifest> {
        manifest_from_schema(&self.service, prim_path)
    }

    /// Attach every enabled runtime-kind authored behavior in the world onto
    /// the session's single host. Returns the count attached. See
    /// [`behavior::attach_authored_behaviors`].
    ///
    /// Module bytes are resolved **through USD's own asset resolution first**:
    /// for each behavior the composed prim is looked up on the session stage and
    /// its `idtx:behavior:module` asset attribute is resolved against its
    /// *authoring* layer, then read via [`StageService::read_asset`] (which also
    /// serves package-internal `foo.usdz[inner.wasm]` paths). This is what makes
    /// a relative `@./behaviors/x.wasm@` resolve identically whether the stage is
    /// opened directly (preview) or through a composed assembly (packaged
    /// player) — the caller's directory-relative `read_module` is kept only as a
    /// fallback.
    ///
    /// The capability manifest is **not** a sidecar any more: it is built
    /// directly from the `IdtxBehaviorAPI` schema attributes on the behavior's
    /// prim ([`manifest_from_schema`]) — the single source of truth, with no
    /// `idtx.toml` read and no fallback.
    ///
    /// Once all instances are attached, [`StageEvent::Loaded`] is broadcast
    /// to the stage-scoped behaviors so they can react to the level coming
    /// up.
    pub fn attach_authored_behaviors(
        &mut self,
        read_module: impl Fn(&str) -> Option<Vec<u8>>,
    ) -> usize {
        let initial = !self.authored_behaviors_enabled;
        self.refresh_behavior_data();
        self.authored_behaviors_enabled = true;
        self.behavior_reconcile_pending = false;
        self.behaviors.set_time_code(behavior::stage_time_code(self.hydrator.world(), self.pseudo_root));
        let (camera_pose, camera_transform) = self.view_camera_pose();
        self.behaviors.set_camera_pose(camera_pose);
        self.behaviors.set_camera_transform(camera_transform);
        use idtx_scene::behavior::attr;

        // Ensure the pseudo-root carries the `StageRoot` marker — the documented
        // ECS-side target for stage-scoped behaviors (the host keys stage events
        // on `WireEntity::NULL`; this marker is its scene-graph counterpart).
        if let Some(root) = self.pseudo_root {
            self.hydrator
                .world_mut()
                .entity_mut(root)
                .insert(idtx_scene::StageRoot);
        }

        let count = {
            // The stage-behavior run-condition gate is resolved **per prim** at
            // attach time (see the closure below): each stage behavior's owning
            // layer run scope vs how that layer composes into this stage. The run
            // scope is read off the layer that *owns* each stage behavior (the
            // payloaded level), not the open stage's root layer — a composed
            // assembly's root carries no run-scope opinion, and different levels
            // may carry different scopes, so a single global permit would wrongly
            // admit or reject all of them together.
            let Self {
                service,
                behaviors,
                hydrator,
                ..
            } = self;
            let service: &StageService = service;

            behavior::attach_authored_behaviors(
                behaviors,
                hydrator.world_mut(),
                // Run-condition gate: entity-scoped behaviors always pass; a
                // stage-scoped behavior passes only when its own owning-layer run
                // scope permits this composition context, resolved per prim path.
                |prim_path: &str| service.stage_behavior_permitted_for_prim(prim_path),
                // USD-primary module reader: resolve `idtx:behavior:module` on
                // the composed prim against its authoring layer and read the
                // bytes through the resolver; fall back to the caller's
                // directory-relative reader.
                |prim_path: &str, module_asset: &str| {
                    read_asset_via_resolver(service, prim_path, attr::MODULE)
                        .or_else(|| read_module(module_asset))
                },
                // Manifest built straight from the IdtxBehaviorAPI schema
                // attributes on the behavior's prim — no sidecar, no fallback.
                |prim_path: &str, _module_asset: &str| manifest_from_schema(service, prim_path),
            )
        };
        // Recompute XR pose single-authority now instances are live: an authored
        // `*_controller_sync` / `*_aim_sync` behavior on a hand prim makes that
        // prim behavior-owned, so `author_hydrated_controller_poses` stands down
        // for it. (Disjoint `&self.hydrator` + `&self.behaviors` reads.)
        #[cfg(all(feature = "xr-preview", feature = "authoring"))]
        {
            self.xr_pose_behavior_owned = [
                self.xr_pose_prim_behavior_owned(xr_controller_prefab::LEFT_CONTROLLER_PATH),
                self.xr_pose_prim_behavior_owned(xr_controller_prefab::RIGHT_CONTROLLER_PATH),
                self.xr_pose_prim_behavior_owned(xr_controller_prefab::LEFT_POINTER_PATH),
                self.xr_pose_prim_behavior_owned(xr_controller_prefab::RIGHT_POINTER_PATH),
            ];
        }
        // Broadcast the stage-load event now that instances are live.
        tracing::info!(
            target: "behavior",
            attached = count,
            total_instances = self.behaviors.instance_count(),
            initial,
            "authored behavior reconciliation complete"
        );
        behavior::initialize_new_bindings(&mut self.behaviors, self.hydrator.world_mut(), self.play_state == PlayState::Playing);
        count
    }

    /// Broadcast a stage-level lifecycle [`StageEvent`] to the stage-scoped
    /// behaviors on the session host. A thin split-borrow wrapper
    /// over [`BehaviorHost::dispatch_stage_event`].
    pub fn dispatch_stage_event(&mut self, event: StageEvent) {
        let Self {
            behaviors,
            hydrator,
            ..
        } = self;
        behaviors.dispatch_stage_event(hydrator.world_mut(), event);
    }

    /// Number of behavior instances attached to the session host.
    #[must_use]
    pub fn attached_behavior_count(&self) -> usize {
        self.behaviors.instance_count()
    }

    pub fn has_live_behavior(&self, path: &str) -> bool {
        self.hydrator.entity_for_path(path).is_some_and(|entity| {
            let world = self.hydrator.world();
            world.get::<PrimNode>(entity).is_none_or(|node| node.state.active && node.state.loaded)
                && world.get::<idtx_scene::behavior::BehaviorRef>(entity).is_none_or(|reference| reference.enabled)
                && self.behaviors.has_live_instance_for_entity(idtx_behavior::entity_to_wire(entity))
        })
    }

    /// Readiness for an incoming playable level: composition must be complete
    /// and every permitted authored runtime module must have attached.
    pub fn validate_runtime_readiness(&self) -> Result<(), SessionError> {
        self.service.validate_runtime_readiness()?;
        for entity in self.hydrator.world().iter_entities() {
            let Some(reference) = entity.get::<idtx_scene::behavior::BehaviorRef>() else { continue; };
            if !reference.enabled || reference.kind != idtx_behavior_abi::BehaviorKind::Runtime { continue; }
            let Some(node) = entity.get::<PrimNode>() else { continue; };
            if !node.state.active || !node.state.loaded { continue; }
            match idtx_scene::resolve_owning_player_role(self.hydrator.world(), entity.id()) {
                idtx_scene::NetRole::Remote => continue,
                idtx_scene::NetRole::Server if !reference.server_authoritative => continue,
                _ => {}
            }
            if reference.stage_scope == idtx_scene::behavior::BehaviorScope::Stage
                && !self.service.stage_behavior_permitted_for_prim(&node.path) { continue; }
            if (reference.module_asset.is_some() || reference.source_asset.is_some())
                && !self.behaviors.has_ready_instance_for_entity(idtx_behavior::entity_to_wire(entity.id())) {
                return Err(StageError::OpenUsd(format!("required behavior did not attach at {}", node.path)).into());
            }
        }
        Ok(())
    }

    /// The single per-frame behavior tick (keystone). When an XR preview
    /// is active it feeds the HMD/controller pose snapshot first, then runs the
    /// host exactly once with the session `play_state`, then sweeps the driven
    /// XR transforms into world matrices. No-op when nothing is attached and no
    /// preview is active. Call once per frame with the frame delta-time in
    /// seconds.
    ///
    /// Returns `true` when a tracked HMD pose was available this frame (always
    /// `false` when no XR preview is active).
    pub fn tick_behaviors(&mut self, dt_seconds: f32) -> bool {
        self.run_behavior_frame(dt_seconds, None)
    }

    /// Platform-adapter input for the same session-owned behavior/frame path.
    pub fn tick_external_behaviors(&mut self, dt_seconds: f32, snapshot: idtx_behavior::XrSnapshot,
        recenter: glam::Vec3, input: idtx_behavior::RawInput) -> bool {
        self.behaviors.set_xr_snapshot(snapshot);
        self.behaviors.set_xr_recenter(recenter);
        self.run_behavior_frame(dt_seconds, Some(input))
    }

    fn run_behavior_frame(&mut self, dt_seconds: f32, external_input: Option<idtx_behavior::RawInput>) -> bool {
        if let Err(error) = self.tick() {
            tracing::warn!(%error, "behavior tick skipped after failed scene projection");
            self.fail_pending_runtime_projection();
            return false;
        }
        if self.authored_behaviors_enabled && self.behavior_reconcile_pending {
            #[cfg(feature = "metrics")]
            let started = std::time::Instant::now();
            let directory = self.source_path.parent().unwrap_or(Path::new(".")).to_owned();
            self.attach_authored_behaviors(|path| std::fs::read(directory.join(path)).ok());
            #[cfg(feature = "metrics")]
            tracing::debug!(elapsed_us = started.elapsed().as_micros() as u64, "behavior reconciliation");
        }
        self.settle_runtime_spawns();
        // Read the stage animation clock before the split-borrow (both
        // borrows end here) so the host's `idtx_time_code` import tracks the
        // same clock that drives geometry/skeleton animation.
        let time_code = behavior::stage_time_code(self.hydrator.world(), self.pseudo_root);

        // Play-space recenter is only meaningful when a local-player pawn exists:
        // `xr_player_move` on `/World/Player` bakes the offset into the root, and
        // the eye/fed-pose compensation cancels it. With no pawn nothing bakes it,
        // so compensating would *shift* the view — hold the offset at ZERO then.
        #[cfg(feature = "xr-preview")]
        let has_local_player = self.local_player_camera_path().is_some();
        #[cfg(feature = "xr-preview")]
        let locomotion_live = self.has_live_behavior(xr_controller_prefab::PLAYER_ROOT_PATH);

        // Seed the active view/player camera pose + world transform so a
        // behavior can raycast from the viewpoint and rigidly attach a grabbed
        // prim to the camera frame (desktop orbit/player camera). Computed
        // before the split-borrow since it reads several `self` fields.
        let (camera_pose, camera_transform) = self.view_camera_pose();

        // Split-borrow the fields the tick touches (mirrors the old `tick_xr`).
        // The XR-only fields are destructured only when the XR platform is
        // compiled in; the behavior host fields are always present.
        let Self {
            #[cfg(feature = "xr-preview")]
            xr_preview,
            behaviors,
            hydrator,
            play_state,
            frame_input,
            #[cfg(feature = "xr-preview")]
            xr_recenter,
            ..
        } = self;

        // Feed the XR pose snapshot (poll HMD + controllers) before the tick.
        // With no preview (or no XR platform compiled in), skip the whole tick
        // when nothing is attached (parity with the retired `BehaviorDriver`).
        #[cfg(feature = "xr-preview")]
        let tracked = match xr_preview.as_mut() {
            Some(xr) => {
                // Update the play-space recenter from this frame's head ground
                // position (instant snap past the threshold), *before* feeding:
                // `feed_snapshot` bakes it into the host (so `xr_player_move`
                // re-bases `/World/Player`) and subtracts it from every fed pose,
                // and `render_xr` subtracts it from the eye — so the view stays
                // fixed while the capsule follows the head. One-frame latency on
                // the head pose, same as the controller feed.
                match (has_local_player, locomotion_live, xr.current_pose()) {
                    (true, true, Some(head)) => {
                        *xr_recenter = recenter_step(
                            *xr_recenter,
                            flatten_ground(head.position),
                            RECENTER_THRESHOLD,
                        );
                    }
                    // No pawn (or no tracked head): never author a non-zero offset.
                    (false, _, _) => *xr_recenter = glam::Vec3::ZERO,
                    _ => {}
                }
                xr.feed_snapshot(behaviors, *xr_recenter)
            }
            None => {
                if behaviors.instance_count() == 0 {
                    return false;
                }
                false
            }
        };
        #[cfg(not(feature = "xr-preview"))]
        let tracked = {
            if behaviors.instance_count() == 0 {
                return false;
            }
            false
        };

        // Feed XR controller buttons / thumbsticks (polled into the platform
        // tracker by `render_xr`) onto the frame input as `Controller/MetaQuest/*`
        // sources, so bound actions resolve from them alongside keyboard / mouse.
        #[cfg(feature = "xr-preview")]
        if let Some(xr) = xr_preview.as_ref() {
            let (left, right) = xr.platform().controller_input();
            let snap = xr_controller_snapshot(left, right);
            idtx_behavior::apply_xr_controllers(frame_input, &snap);
        }

        // Recognise hand gestures from this frame's tracked hand joints and
        // press them onto the frame input as `Gesture/<kind>/<hand>` sources
        // (e.g. a pinch → `Gesture/pinch/right`), *before* the single input
        // feed below, so a gesture-bound action (`right_grab`, …) resolves
        // alongside keyboard / controller input.
        #[cfg(all(feature = "xr-preview", feature = "gesture-recognition"))]
        if let Some(xr) = xr_preview.as_mut() {
            xr.pump_gestures(frame_input);
        }

        // Feed this frame's device input into the host router, then
        // run the frame, then roll the accumulator so button edges resolve
        // correctly next frame and per-frame pointer deltas reset.
        behaviors.update_input(&external_input.unwrap_or_else(|| frame_input.raw()), dt_seconds);

        // The one and only `run_frame` per session per frame.
        behaviors.set_time_code(time_code);
        behaviors.set_camera_pose(camera_pose);
        behaviors.set_camera_transform(camera_transform);
        behaviors.run_frame(hydrator.world_mut(), dt_seconds, *play_state);

        frame_input.end_frame();

        // Drain the playback-transport commands the behaviors posted
        // this frame and apply each to the addressed entity's `AnimClipPlayer`.
        // The host cannot do this itself (it does not depend on `idtx-scene`),
        // so the session — which links both — is the join point. The mutated
        // playheads are advanced + resampled by `tick_clip_animation` on the
        // next `tick_animation` (animation ticks before behaviors in the frame
        // loop), matching the existing one-frame settle of authored transport.
        let playback_commands = behaviors.drain_playback_commands();
        if !playback_commands.is_empty() {
            apply_playback_commands(hydrator.world_mut(), &playback_commands);
        }

        // Drain the deferred session-layer edits the behaviors posted this
        // frame. They must be applied through `self.service` (authoring writes +
        // re-projection), which is not part of the split-borrow above — so collect
        // them here and apply after the borrow ends.
        let session_edits = behaviors.drain_session_edits();

        // Grab suspend/resume: drain the physics-control commands behaviors
        // posted this frame. Applied to `self.physics` after the borrow ends,
        // which is *before* `tick_physics` in the frame loop — so a suspend takes
        // effect the same frame and the solver stops fighting the grab. The host
        // has no physics handle; the session (which links the subsystem) is the
        // join point, exactly like the playback / session-edit drains above.
        let physics_control = behaviors.drain_physics_control_commands();

        // Drain the transient overlay commands and resolve each
        // highlight's target entity → USD path (needs the hydrator, still
        // borrowed here). Rendered as coloured boxes by `render_one_frame`.
        let overlay_commands = behaviors.drain_overlay_commands();
        let highlights = resolve_overlay_highlights(hydrator, &overlay_commands);
        // World-space draw lines (teleport arc + landing marker) plus the ABI-3
        // widget hit-point cue rings, assembled by the shared helper so this
        // inline path and the externally-owned runtime path
        // (`resolve_overlay_highlights_external`) can never drift — the headset
        // cue bug came from exactly that drift (only this path built the rings).
        let widget_markers = behaviors.drain_widget_markers();
        let overlay_lines = assemble_behavior_overlay_lines(
            &overlay_commands,
            &widget_markers,
            self.world_unit_scale.metres_per_unit(),
        );

        // Drain the deferred level-transition requests a `level_control`
        // behavior posted this frame. Surfaced — not acted on — here: the runtime
        // driver polls `pending_level_transition` and performs the swap. The
        // request carries only `(target, spawn)`, so no hydrator is needed; keep
        // the most recent if several arrived this frame.
        let level_transition = behaviors.drain_level_transitions().pop();
        let level_cancellation = behaviors.drain_level_cancellation();

        #[cfg(feature = "xr-preview")]
        if let Some(xr) = xr_preview.as_mut() {
            xr.sweep(hydrator.world_mut());
        }

        // Split-borrow bindings are no longer used past this point, so `self` is
        // free again for the authoring apply path.
        self.apply_session_edits(&session_edits);
        self.settle_runtime_spawns();
        self.apply_physics_control(&physics_control);
        self.behavior_highlights = highlights;
        self.behavior_overlay_lines = overlay_lines;
        // Surface the latest level-transition request (if any) for the runtime
        // driver to poll; a frame with no request leaves the field untouched so a
        // previously-surfaced-but-unpolled request is not dropped.
        if let Some(req) = level_transition {
            self.pending_level_transition = Some(req);
        }
        self.pending_level_cancellation |= level_cancellation;

        tracked
    }

    /// Take the pending level-transition request a `level_control` behavior
    /// posted, clearing it. The runtime transition driver polls this
    /// after [`Self::tick_behaviors`] and performs the actual session swap;
    /// returns `None` when no request is outstanding. Deliberately **not**
    /// `authoring`-gated — the shipped runtime reads it.
    pub fn take_pending_level_transition(
        &mut self,
    ) -> Option<idtx_behavior::LevelTransitionRequest> {
        self.pending_level_transition.take()
    }

    pub fn take_pending_level_cancellation(&mut self) -> bool {
        std::mem::take(&mut self.pending_level_cancellation)
    }

    /// Post a pending level-transition request directly, bypassing the
    /// `level_control` behavior path that normally populates it in
    /// [`Self::tick_behaviors`].
    ///
    /// This is the host/test seam the runtime driver and its tests use to
    /// drive a transition without a behavior round-trip, and the hook a future
    /// net-authoritative peer would post through. Deliberately **not**
    /// `authoring`-gated — it only buffers the `(target, spawn)` the driver
    /// polls, exactly like the behavior-posted path. A previously-posted request
    /// that has not been taken is overwritten.
    pub fn post_pending_level_transition(
        &mut self,
        request: idtx_behavior::LevelTransitionRequest,
    ) {
        self.pending_level_transition = Some(request);
    }

    /// Apply the deferred session-layer edits a `stage_edit`-capable behavior
    /// posted this frame, then re-project once so the ECS/render worlds
    /// reflect them the same frame. Each edit's `target` is a wire entity — the
    /// same `bevy_ecs::Entity` re-encoded — resolved to its authored USD path
    /// via [`PrimNode`]; an unresolved target or a path miss is a no-op.
    /// `SetTransform` authors the prim's local transform (guest transforms are
    /// local-space — no `projection_basis` premultiply); `SetVisibility` authors
    /// the session-layer visibility opinion. `DefinePrim` (target = parent) and
    /// `RemovePrim` (target = the prim) route through the structural
    /// `define_prim` / `delete_prim` methods, which re-project topology
    /// themselves. The single `self.tick()` after the batch re-projects
    /// (guarded on at least one successful write).
    #[cfg(feature = "authoring")]
    fn apply_session_edits(&mut self, edits: &[SessionEditCommand]) {
        use idtx_scene::bevy_ecs::entity::Entity;
        if edits.is_empty() {
            return;
        }
        let mut applied = 0usize;
        // Visibility edits are re-asserted against the ECS override chain after
        // the re-projection below (see the trailing block); collect their
        // resolved paths as we go.
        let mut visibility_edits: Vec<(String, bool)> = Vec::new();
        // Parent (camera) paths of any `AttachHud` applied this batch. The HUD
        // child is authored via `define_session_prim`, which drains its own
        // TOPOLOGY notice to mint the PrimId — so the trailing `tick()` below
        // sees no delta and the hydrator never spawns an entity for the new
        // `IdtxHud` prim. We re-project these subtrees explicitly afterwards so
        // `hydrate_hud` can stamp the `HudComp` the render pass consumes.
        let mut hud_parent_paths: Vec<String> = Vec::new();
        for edit in edits {
            let Some(entity) = Entity::try_from_bits(edit.target.to_bits()) else {
                self.reject_runtime_spawn(&edit.op, idtx_behavior_abi::ErrorCode::StaleHandle);
                continue;
            };
            let Some(path) = self
                .hydrator
                .world()
                .get::<PrimNode>(entity)
                .map(|n| n.path.clone())
            else {
                self.reject_runtime_spawn(&edit.op, idtx_behavior_abi::ErrorCode::StaleHandle);
                continue;
            };
            // Dedup redundant visibility: a behavior that re-asserts the same
            // visibility every frame (a gaze cursor held visible while aiming)
            // must not re-author it — a VISIBILITY write forces a full re-extract
            // (both via the non-transform reprojection and the explicit
            // `snapshot_dirty` below). Skip when the value is unchanged since the
            // last author; the edge (show↔hide) still authors + reprojects once.
            if let SessionEditOp::SetVisibility(visible) = &edit.op
                && self.session_visibility_authored.get(&path) == Some(visible)
            {
                continue;
            }
            let result = match &edit.op {
                SessionEditOp::SpawnReference { .. } => self.apply_runtime_spawn(&path, &edit.op),
                SessionEditOp::SetTransform(x) => {
                    let matrix = x.to_matrix().to_cols_array().map(f64::from);
                    self.service
                        .author_session_transform(&path, matrix)
                        .map_err(SessionError::from)
                }
                SessionEditOp::SetVisibility(visible) => {
                    // Record the resolved path so the ECS override chain can be
                    // re-asserted after the tick (the USD author alone does not
                    // drive the renderer — see the trailing block).
                    self.session_visibility_authored
                        .insert(path.clone(), *visible);
                    visibility_edits.push((path.clone(), *visible));
                    self.service
                        .author_session_visibility(&path, *visible)
                        .map_err(SessionError::from)
                }
                // Guest structure belongs to the session layer. The trailing
                // tick hydrates its retained notices; weaker prims are masked
                // by deactivation rather than edited in their source layer.
                SessionEditOp::DefinePrim { type_name, name } => {
                    if name.is_empty() {
                        Ok(())
                    } else {
                        let child = format!("{}/{}", path.trim_end_matches('/'), name);
                        self.service.mutate_session_prim(&child, type_name, false).map_err(SessionError::from)
                    }
                }
                SessionEditOp::RemovePrim => self.remove_runtime_prim(&path),
                // Atomic HUD attach: define an `IdtxHud` child in the SESSION
                // layer under the target (camera) and author its `idtx:hud:*`
                // parameters there, so a behavior needs no round-trip to the new
                // prim's entity. `size_px` (0,0) means "use the native image
                // size"; anchor defaults to `center` (schema fallback).
                SessionEditOp::AttachHud {
                    name,
                    image,
                    size_px,
                    opacity,
                } => {
                    if name.is_empty() {
                        Ok(())
                    } else {
                        let child = format!("{}/{}", path.trim_end_matches('/'), name);
                        let r = self.attach_session_hud(&child, image, *size_px, *opacity);
                        if r.is_ok() {
                            hud_parent_paths.push(path.clone());
                        }
                        r
                    }
                }
                // Author a string into an `IdtxWidgetField*` child's
                // `idtx:widget:value` in the session layer. The child already
                // composes in the stage (it is defined in the asset), so no
                // define is needed — just author the property at
                // `<panel>/<name>`. Because the property is in the
                // `idtx:widget:` namespace, the trailing `tick()` drains a
                // value-only change that `tick`'s `widget_changed` path turns
                // into a `hydrate_widgets` + `patch_widget_panels` re-raster.
                SessionEditOp::SetWidgetValue { name, value } => {
                    if name.is_empty() {
                        Ok(())
                    } else {
                        let child = format!("{}/{}", path.trim_end_matches('/'), name);
                        self.service
                            .author_session_property(
                                &child,
                                idtx_usd_extension::ATTR_WIDGET_VALUE,
                                &idtx_usd::PropertyValue::String(value.clone()),
                            )
                            .map_err(SessionError::from)
                    }
                }
            };
            match result {
                Ok(()) => applied += 1,
                Err(e) => tracing::warn!("apply guest session edit on {path}: {e}"),
            }
        }
        if applied > 0 {
            if let Err(e) = self.tick() {
                tracing::warn!("reproject after guest session edits: {e}");
                self.fail_pending_runtime_projection();
            }
        }
        // Hydrate any `IdtxHud` child just created by an `AttachHud`. Its
        // `define_session_prim` drained the TOPOLOGY notice internally, so the
        // `tick()` above never re-projected it into the ECS. Re-project the
        // camera subtree(s) the HUD hangs under so the hydrator spawns the new
        // prim's entity, then stamp its `HudComp` — otherwise the crosshair is
        // authored into the session layer but never reaches the render snapshot.
        if !hud_parent_paths.is_empty() {
            hud_parent_paths.sort();
            hud_parent_paths.dedup();
            match self.service.project_topology_paths(&hud_parent_paths) {
                Ok(delta) => {
                    self.hydrator.apply_topology_delta(&delta, &self.service);
                    if let Ok(projection) = self.service.project_viewport() {
                        let _ = self.hydrator.apply_viewport_projection(&projection);
                    }
                    self.hydrator.hydrate_hud(&self.service);
        self.hydrator.hydrate_widgets(&self.service);
                    self.snapshot_dirty = true;
                }
                Err(e) => tracing::warn!("reproject HUD subtree after attach: {e}"),
            }
        }
        // Re-assert visibility against the ECS override chain AFTER the tick.
        // `author_session_visibility` only flips the composed USD opinion, but
        // the renderer reads `VisibilityComp.visible`, which is derived from the
        // ECS `VisibilityOverride` chain — the viewport projection leaves that
        // chain untouched, and `initialize_visibility_overrides` preserves it
        // once seeded. Without this, a behavior's `set_visibility(true)` on an
        // authored-`invisible` prim never surfaces: the entity keeps its seeded
        // `VisibilityOverride::Invisible` and the renderer hides it, even though
        // the composed USD now says `inherited`. This is the same pitfall
        // `apply_level_transition` documents and re-asserts after its own tick.
        // `set_prim_visibility_override` authors an explicit override that wins
        // even under an invisible ancestor (re-resolve by path, since a batched
        // topology edit may have re-hydrated the entity).
        let mut vis_changed = false;
        for (path, visible) in &visibility_edits {
            if let Some(entity) = self.hydrator.entity_for_path(path) {
                self.hydrator.set_prim_visibility_override(entity, *visible);
                vis_changed = true;
            }
        }
        if vis_changed {
            self.visibility_generation = self.visibility_generation.wrapping_add(1);
            self.snapshot_dirty = true;
        }
    }

    /// Test-only seam: build the `SessionEditCommand` a guest `set_visibility`
    /// would post — targeting the prim at `path`'s hydrated entity — and run it
    /// through the real [`apply_session_edits`](Self::apply_session_edits) path.
    /// Lets a regression test exercise the guest visibility apply without a wasm
    /// host. Not part of the stable API. No-op if `path` is not hydrated.
    #[cfg(feature = "authoring")]
    #[doc(hidden)]
    pub fn apply_guest_visibility_edit_for_test(&mut self, path: &str, visible: bool) {
        let Some(entity) = self.hydrator.entity_for_path(path) else {
            return;
        };
        let cmd = SessionEditCommand {
            target: idtx_behavior_abi::Entity::from_bits(entity.to_bits()),
            op: SessionEditOp::SetVisibility(visible),
        };
        self.apply_session_edits(&[cmd]);
    }

    /// Test-only seam: build the `SessionEditCommand` a guest `attach_crosshair`
    /// would post — an [`SessionEditOp::AttachHud`] targeting the camera prim at
    /// `camera_path`'s hydrated entity — and run it through the real
    /// [`apply_session_edits`](Self::apply_session_edits) path. Exercises the
    /// behavior-driven HUD attach (define session prim + author params +
    /// re-project + hydrate) without a wasm host. Not part of the stable API.
    /// No-op if `camera_path` is not hydrated.
    #[cfg(feature = "authoring")]
    #[doc(hidden)]
    pub fn apply_guest_hud_attach_for_test(
        &mut self,
        camera_path: &str,
        name: &str,
        image: &str,
        size_px: [i32; 2],
        opacity: f32,
    ) {
        let Some(entity) = self.hydrator.entity_for_path(camera_path) else {
            return;
        };
        let cmd = SessionEditCommand {
            target: idtx_behavior_abi::Entity::from_bits(entity.to_bits()),
            op: SessionEditOp::AttachHud {
                name: name.to_owned(),
                image: image.to_owned(),
                size_px,
                opacity,
            },
        };
        self.apply_session_edits(&[cmd]);
    }

    /// Test-only seam: the hydrated [`idtx_scene::HudComp::image`] on the prim at
    /// `path`, or `None` when the prim has no entity or no `HudComp`. Lets a
    /// regression test assert a behavior-attached HUD actually hydrated into the
    /// ECS (and therefore reaches the render snapshot). Not part of the stable API.
    #[cfg(feature = "authoring")]
    #[doc(hidden)]
    #[must_use]
    pub fn hud_image_for_test(&self, path: &str) -> Option<String> {
        let entity = self.hydrator.entity_for_path(path)?;
        self.hydrator
            .world()
            .get::<idtx_scene::HudComp>(entity)
            .map(|h| h.image.clone())
    }

    /// Test-only seam: build the [`SessionEditOp::SetWidgetValue`] a guest
    /// `set_widget_value` would post — targeting the `IdtxWidgetPanel` at
    /// `panel_path`'s hydrated entity, with the panel-relative `child_name` — and
    /// run it through the real [`apply_session_edits`](Self::apply_session_edits)
    /// path. Exercises the behavior-driven widget-value author (author session
    /// property → re-project → `widget_changed` → `hydrate_widgets`) without a
    /// wasm host. No-op if `panel_path` is not hydrated. Not part of the stable API.
    #[cfg(feature = "authoring")]
    #[doc(hidden)]
    pub fn apply_guest_widget_value_edit_for_test(
        &mut self,
        panel_path: &str,
        child_name: &str,
        value: &str,
    ) {
        let Some(entity) = self.hydrator.entity_for_path(panel_path) else {
            return;
        };
        let cmd = SessionEditCommand {
            target: idtx_behavior_abi::Entity::from_bits(entity.to_bits()),
            op: SessionEditOp::SetWidgetValue {
                name: child_name.to_owned(),
                value: value.to_owned(),
            },
        };
        self.apply_session_edits(&[cmd]);
    }

    /// Test-only seam: the composed `idtx:widget:value` string authored on the
    /// `IdtxWidgetFieldString` prim at `path`, or `None` when the prim has no
    /// entity or the attribute is absent. Lets a regression test assert a
    /// behavior-driven value reached the session layer. Not part of the stable API.
    #[cfg(feature = "authoring")]
    #[doc(hidden)]
    #[must_use]
    pub fn widget_value_for_test(&self, path: &str) -> Option<String> {
        let prim = self
            .hydrator
            .entity_for_path(path)
            .and_then(|e| self.hydrator.world().get::<idtx_scene::PrimNode>(e))
            .map(|node| node.prim)?;
        Some(
            self.service
                .eval_attribute(prim, idtx_usd_extension::ATTR_WIDGET_VALUE, 0.0, true)
                .ok()
                .flatten()?
                .value_repr,
        )
    }
    /// [`PhysicsSubsystem::set_body_simulated`] path a `physics`-capable
    /// behavior's grab (`suspend`) / release (`resume`) posts. Returns `true`
    /// when a body changed state; a no-op `false` when the subsystem is not yet
    /// built (call [`Self::tick_physics`] first) or the prim is not a tracked
    /// body. Not part of the stable API.
    #[cfg(feature = "physics")]
    #[doc(hidden)]
    pub fn set_physics_body_simulated_for_test(&mut self, path: &str, simulated: bool) -> bool {
        let Some(entity) = self.hydrator.entity_for_path(path) else {
            return false;
        };
        let Some(sub) = self.physics.as_mut() else {
            return false;
        };
        sub.set_body_simulated(entity, simulated)
    }

    /// Test-only seam: `(body_count, collider_count)` of the built physics world,
    /// or `None` when the subsystem is not built yet. A compound body contributes
    /// one body and more than one collider. Not part of the stable API.
    #[cfg(feature = "physics")]
    #[doc(hidden)]
    pub fn physics_body_counts_for_test(&self) -> Option<(usize, usize)> {
        self.physics.as_ref().map(|s| s.counts())
    }

    /// Whether the next physics tick must perform whole-stage reconciliation.
    /// Test seam for the physics-neutral spawn fast path. Not part of the stable API.
    #[cfg(feature = "physics")]
    #[doc(hidden)]
    pub fn physics_reconciliation_pending_for_test(&self) -> bool {
        self.physics_build_gen != Some(self.topology_generation)
    }

    /// Test-only seam: the hydrated/composed USD type name of the prim at `path`
    /// (`"Cube"`, `"Sphere"`, …), or `""` when the path has no hydrated entity.
    /// Backs the Phase-1 regression that the dynamic-pose write-back must NOT
    /// retype a simulated gprim to `Xform` (a `DefinePrim(path, "Xform")` in the
    /// session layer would, dropping its render mesh + highlight). Not part of
    /// the stable API.
    #[cfg(feature = "physics")]
    #[doc(hidden)]
    pub fn composed_prim_type_for_test(&self, path: &str) -> String {
        self.hydrator
            .entity_for_path(path)
            .and_then(|e| self.hydrator.world().get::<idtx_scene::PrimNode>(e))
            .map(|node| node.type_name.clone())
            .unwrap_or_default()
    }

    /// Test-only seam: whether the physics body at `path` is currently asleep in
    /// the engine (the signal the Phase 2A write-back gate reads), or `None` when
    /// the subsystem is not built or `path` is not a tracked body. Not stable API.
    #[cfg(feature = "physics")]
    #[doc(hidden)]
    pub fn physics_body_sleeping_for_test(&self, path: &str) -> Option<bool> {
        let entity = self.hydrator.entity_for_path(path)?;
        self.physics.as_ref()?.is_entity_sleeping(entity)
    }

    /// Test-only seam: live linear/angular velocity, for reconciliation regressions.
    /// Not part of the stable API.
    #[cfg(feature = "physics")]
    #[doc(hidden)]
    pub fn physics_body_velocity_for_test(&self, path: &str) -> Option<(glam::Vec3, glam::Vec3)> {
        let entity = self.hydrator.entity_for_path(path)?;
        self.physics.as_ref()?.entity_velocity(entity)
    }

    /// Test-only seam: the composed `physics:startsAsleep` bool authored on the
    /// prim at `path` (the USD sleep-sync write-back), or `None` when absent /
    /// unparseable. Backs the Phase 2A assertion that a body that falls asleep has
    /// its rest state mirrored into USD. Not stable API.
    #[cfg(feature = "physics")]
    #[doc(hidden)]
    pub fn physics_starts_asleep_authored_for_test(&self, path: &str) -> Option<bool> {
        let prim = self
            .hydrator
            .entity_for_path(path)
            .and_then(|e| self.hydrator.world().get::<idtx_scene::PrimNode>(e))
            .map(|node| node.prim)?;
        let repr = self
            .service
            .eval_attribute(prim, idtx_usd_extension::ATTR_PHYSICS_STARTS_ASLEEP, 0.0, true)
            .ok()
            .flatten()?
            .value_repr;
        match repr.trim() {
            "1" | "true" | "True" => Some(true),
            "0" | "false" | "False" => Some(false),
            _ => None,
        }
    }

    /// Atomically author a session-layer `IdtxHud` child at `path` plus its
    /// parameters — the apply of [`SessionEditOp::AttachHud`]. Defines the prim
    /// in the session layer if absent (so a behavior may call it every frame
    /// idempotently), then authors `idtx:hud:image` / `idtx:hud:sizePx` /
    /// `idtx:hud:opacity`. `size_px` `[0, 0]` means "use the image's native
    /// size"; anchor is left at the schema default (`center`). Nothing touches
    /// the root layer, so the saved assembly stays clean.
    #[cfg(feature = "authoring")]
    fn attach_session_hud(
        &mut self,
        path: &str,
        image: &str,
        size_px: [i32; 2],
        opacity: f32,
    ) -> Result<(), SessionError> {
        use idtx_usd::PropertyValue;
        if self.service.prim_at_path(path).is_none() {
            self.service
                .define_session_prim(path, idtx_usd_extension::HUD_SCHEMA)?;
            self.topology_generation = self.topology_generation.wrapping_add(1);
        }
        self.service.author_session_property(
            path,
            idtx_usd_extension::ATTR_HUD_IMAGE,
            &PropertyValue::Asset(image.to_owned()),
        )?;
        self.service.author_session_property(
            path,
            idtx_usd_extension::ATTR_HUD_SIZE_PX,
            &PropertyValue::Int2(size_px),
        )?;
        self.service.author_session_property(
            path,
            idtx_usd_extension::ATTR_HUD_OPACITY,
            &PropertyValue::F32(opacity),
        )?;
        Ok(())
    }

    /// Non-authoring builds have no session-layer write path, so guest edits are
    /// dropped (the `stage_edit` capability should not be granted in such a
    /// build; this is a defensive no-op with a debug trace).
    #[cfg(not(feature = "authoring"))]
    fn apply_session_edits(&mut self, edits: &[SessionEditCommand]) {
        for edit in edits { self.reject_runtime_spawn(&edit.op, idtx_behavior_abi::ErrorCode::ReadOnly); }
        if !edits.is_empty() {
            tracing::debug!(
                "dropping {} guest session edit(s): authoring feature disabled",
                edits.len()
            );
        }
    }

    /// Apply the physics-control commands (grab suspend/resume) a `physics`-capable
    /// behavior posted this frame to the live physics subsystem. Runs in
    /// `tick_behaviors`, *before* `tick_physics` the same frame, so a suspend
    /// stops the solver authoring the grabbed body immediately. Each command's
    /// `target` is a wire entity (the same `bevy_ecs::Entity` re-encoded).
    ///
    /// Commands that arrive before the subsystem is built (lazy, on the first
    /// `tick_physics`) are **retained**, not dropped — a first-frame grab posts
    /// `suspend` only once (on press) and never retries, so losing it would let
    /// physics overwrite the grab indefinitely. They are replayed right after the
    /// build in `tick_physics`. A command for an entity that is not a tracked body
    /// is a no-op once applied.
    #[cfg(feature = "physics")]
    fn apply_physics_control(&mut self, commands: &[PhysicsControlCommand]) {
        use idtx_scene::bevy_ecs::entity::Entity;
        let Some(sub) = self.physics.as_mut() else {
            // Subsystem not built yet: retain for replay after the lazy build.
            self.pending_physics_control.extend_from_slice(commands);
            return;
        };
        for cmd in commands {
            if let Some(entity) = Entity::try_from_bits(cmd.target.to_bits()) {
                sub.set_body_simulated(entity, cmd.simulated);
            }
        }
    }

    /// Non-physics builds (the in-process editor) never simulate, so grab
    /// suspend/resume commands are dropped — the grab behavior's own
    /// `set_transform` already rides the prim with no solver to fight it.
    #[cfg(not(feature = "physics"))]
    fn apply_physics_control(&mut self, commands: &[PhysicsControlCommand]) {
        let _ = commands;
    }

    /// Apply grab suspend/resume (physics-control) commands drained from a
    /// behavior host the embedder drives **itself**.
    ///
    /// The desktop/`OpenXR` runtime posts these through [`Self::tick_behaviors`],
    /// which drains the session's own [`BehaviorHost`] and calls the internal
    /// `apply_physics_control` for us. The visionOS immersive runtime instead
    /// drives its *own* host (the session's authored-behavior attach loop is
    /// `OpenXR`-feature-gated and unavailable there), so it cannot use that path:
    /// it drains [`BehaviorHost::drain_physics_control_commands`] and hands the
    /// commands here. Call once per frame after that host's `run_frame` and
    /// **before** [`Self::tick_physics`], so a grab suspends native simulation
    /// the same frame and the solver does not fight the grab — exactly the order
    /// `tick_behaviors` guarantees internally. A no-op on a non-`physics` build.
    pub fn apply_physics_control_commands(&mut self, commands: &[PhysicsControlCommand]) {
        self.apply_physics_control(commands);
    }

    /// Apply playback-transport commands drained from a behavior host the
    /// embedder drives **itself** (the visionOS immersive runtime) — the analogue
    /// of the playback drain [`Self::tick_behaviors`] performs internally. Mutates
    /// each addressed entity's `AnimClipPlayer`; the next [`Self::tick_animation`]
    /// resamples the moved playheads (one-frame settle, as on the canonical path).
    /// A no-op on a non-`authoring`/non-playback build or for an empty batch.
    pub fn apply_playback_commands(&mut self, commands: &[PlaybackCommand]) {
        apply_playback_commands(self.hydrator.world_mut(), commands);
    }

    /// Apply deferred session-layer edits drained from a behavior host the
    /// embedder drives **itself** — the visionOS immersive runtime analogue of
    /// the drain [`Self::tick_behaviors`] performs internally.
    ///
    /// `xr_player_move` (and every other `stage_edit`-capable behavior) posts its
    /// pawn-root transform — locomotion, snap-turn, **and the play-space recenter
    /// bake** — as a deferred [`SessionEditOp::SetTransform`], never an immediate
    /// ECS write. The desktop/`OpenXR` path drains + applies these inside
    /// `tick_behaviors`; the visionOS runtime drives its own host, so it must
    /// drain [`BehaviorHost::drain_session_edits`] and hand the commands here.
    /// Call once per frame after that host's `run_frame` and **before** the
    /// render, so the re-based root (and the child camera the viewer anchor reads)
    /// reprojects the same frame. Omitting it strands the recenter: the renderer
    /// still subtracts the per-eye recenter offset with no compensating root bake,
    /// freezing the view in place (no room-scale walking). A no-op on a
    /// non-`authoring` build.
    pub fn apply_session_edit_commands(&mut self, edits: &[SessionEditCommand]) {
        self.apply_session_edits(edits);
    }

    /// Mutable access to the active preview session (e.g. to inject a mock
    /// pose in tests). `None` when no preview is running.
    #[cfg(feature = "xr-preview")]
    pub fn xr_preview_mut(&mut self) -> Option<&mut xr_preview::XrPreviewSession> {
        self.xr_preview.as_mut()
    }

    /// Enable hand-gesture recognition on the active XR preview.
    ///
    /// Spawns the background recogniser; recognised gestures are routed into
    /// the behavior input router as `Gesture/<kind>/<hand>` sources. Returns
    /// `false` when no preview is active (start the preview first).
    #[cfg(all(feature = "gesture-recognition", feature = "xr-preview"))]
    pub fn enable_gesture_recognition(
        &mut self,
        cfg: idtx_platform_xr::GestureConfig,
    ) -> bool {
        match self.xr_preview.as_mut() {
            Some(xr) => {
                xr.enable_gesture_recognition(cfg);
                true
            }
            None => false,
        }
    }

    /// Enable hand-gesture recognition with the default [`GestureConfig`].
    ///
    /// A convenience for callers (e.g. `idtx-runtime-core`) that do not depend
    /// on `idtx-platform-xr` and so cannot name [`GestureConfig`]. Returns
    /// `false` when no preview is active (start the preview first).
    #[cfg(all(feature = "gesture-recognition", feature = "xr-preview"))]
    pub fn enable_gesture_recognition_default(&mut self) -> bool {
        self.enable_gesture_recognition(idtx_platform_xr::GestureConfig::default())
    }

    /// The gestures recognised on the most recent [`Self::tick_xr`], or an
    /// empty slice when no preview is active or recognition is disabled.
    #[cfg(all(feature = "gesture-recognition", feature = "xr-preview"))]
    #[must_use]
    pub fn xr_gesture_events(&self) -> &[idtx_platform_xr::GestureEvent] {
        self.xr_preview
            .as_ref()
            .map_or(&[], xr_preview::XrPreviewSession::last_gesture_events)
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────────

/// Merge `new` deferred transform patches into `pending` (latest value per path
/// wins). See [`StudioSession::merge_transform_patches`] for why reprojection
/// passes must accumulate rather than overwrite. Free function so the merge is
/// unit-testable without a full session.
fn merge_transform_patches_into(
    pending: &mut Option<Vec<(String, [f32; 16])>>,
    new: Vec<(String, [f32; 16])>,
) {
    if new.is_empty() {
        return;
    }
    match pending.as_mut() {
        Some(existing) => {
            for (path, matrix) in new {
                if let Some(slot) = existing.iter_mut().find(|(p, _)| *p == path) {
                    slot.1 = matrix;
                } else {
                    existing.push((path, matrix));
                }
            }
        }
        None => *pending = Some(new),
    }
}

/// Convert an openxr-sys boundary `ControllerInput` into the platform-xr
/// `ControllerButtons` the platform tracker holds. Field-for-field; both are
/// device-neutral plain data.
#[cfg(idtx_xr)]
fn controller_buttons(
    input: &idtx_openxr_sys::ControllerInput,
) -> idtx_platform_xr::ControllerButtons {
    idtx_platform_xr::ControllerButtons {
        thumbstick: input.thumbstick,
        trigger: input.trigger,
        grip: input.grip,
        thumbstick_click: input.thumbstick_click,
        primary: input.primary,
        secondary: input.secondary,
        menu: input.menu,
    }
}

/// Default play-space recenter threshold (metres). When the HMD's ground
/// distance from the current pawn origin exceeds this, the origin snaps under the
/// headset. "Quite small" per the feature request — large enough to ignore
/// normal sway, small enough that the body never visibly trails the head.
pub const RECENTER_THRESHOLD: f32 = 0.05;

/// Project a position onto the ground plane (drop its height / Y component).
#[must_use]
pub fn flatten_ground(p: glam::Vec3) -> glam::Vec3 {
    glam::Vec3::new(p.x, 0.0, p.z)
}

/// Advance the play-space recenter offset for one frame (instant snap).
///
/// `current` is the held offset, `head_flat` the HMD's ground-projected position
/// (stage space, y = 0). When the head has drifted past `threshold` from the
/// current origin, snap the offset to the head (so the pawn — and its capsule —
/// re-base under the headset); otherwise hold. The result is always on the ground
/// plane. Pure so it unit-tests without a headset, and shared verbatim by the
/// `OpenXR` tick and the visionOS runtime so the rule is identical on both.
#[must_use]
pub fn recenter_step(current: glam::Vec3, head_flat: glam::Vec3, threshold: f32) -> glam::Vec3 {
    let head_flat = flatten_ground(head_flat);
    if (head_flat - current).length() > threshold {
        head_flat
    } else {
        current
    }
}

/// Derive the HMD (head) pose from the two located eye poses so the XR behavior
/// snapshot carries a tracked head.
///
/// `OpenXR` reports a pose per eye, not a head pose; both eyes share the headset
/// orientation, so the head centre is the midpoint of the two eye positions and
/// the head orientation is either eye's (the left is used). Pure so it is
/// unit-testable without a live XR session. The `timestamp` is informational and
/// left `0.0` — gaze-relative locomotion integrates against the host frame `dt`,
/// not the pose timestamp.
#[cfg(idtx_xr)]
fn head_pose_from_eyes(
    left_eye: idtx_openxr_sys::LocatedPose,
    right_eye: idtx_openxr_sys::LocatedPose,
) -> idtx_platform_xr::HmdPose {
    idtx_platform_xr::HmdPose {
        position: (glam::Vec3::from(left_eye.position) + glam::Vec3::from(right_eye.position))
            * 0.5,
        orientation: glam::Quat::from_array(left_eye.orientation),
        timestamp: 0.0,
    }
}

/// Build the behavior-layer `XrControllerSnapshot` from the platform tracker's
/// per-hand `ControllerButtons`, ready to feed onto `FrameInput` as
/// `Controller/MetaQuest/*` sources.
#[cfg(feature = "xr-preview")]
fn xr_controller_snapshot(
    left: &idtx_platform_xr::ControllerButtons,
    right: &idtx_platform_xr::ControllerButtons,
) -> idtx_behavior::XrControllerSnapshot {
    let to_hand = |b: &idtx_platform_xr::ControllerButtons| idtx_behavior::XrHandInput {
        thumbstick: b.thumbstick,
        trigger: b.trigger,
        grip: b.grip,
        thumbstick_click: b.thumbstick_click,
        primary: b.primary,
        secondary: b.secondary,
        menu: b.menu,
    };
    idtx_behavior::XrControllerSnapshot {
        left: to_hand(left),
        right: to_hand(right),
    }
}

/// Apply behavior-emitted [`PlaybackCommand`]s to the `AnimClipPlayer` playheads
/// they address.
///
/// This is the join between the behavior host (which owns the command queue but
/// not `idtx-scene`) and the scene clip clock. Each command's `target` is a wire
/// [`idtx_behavior::abi`] entity — the same `bevy_ecs::Entity` re-encoded via
/// `to_bits`, since the host runs inside the scene world — so it is resolved by
/// `Entity::try_from_bits` + a live `get_mut::<AnimClipPlayer>`. A command whose
/// target has no clip player, or whose `clip` label names no existing playhead,
/// is a no-op: a `Play`/`Seek` cannot synthesise a playhead because the command
/// carries no time range or target subtrees (those come from clip hydration).
/// The transport semantics mirror
/// [`idtx_scene::apply_playback_request`] applied per playhead.
fn apply_playback_commands(
    world: &mut idtx_scene::bevy_ecs::world::World,
    commands: &[PlaybackCommand],
) {
    for cmd in commands {
        let Some(entity) =
            idtx_scene::bevy_ecs::entity::Entity::try_from_bits(cmd.target.to_bits())
        else {
            continue;
        };
        let Some(mut player) = world.get_mut::<AnimClipPlayer>(entity) else {
            continue;
        };
        let Some(head) = player.playhead_mut(&cmd.clip) else {
            continue;
        };
        match cmd.kind {
            PlaybackCommandKind::Play => {
                // Rewind a finished, non-looping head so `Play` restarts it
                // (mirrors `PlaybackRequest::Play`).
                if !head.looping
                    && head.has_range()
                    && (head.current_time_code - head.end_time_code).abs() < 1e-9
                {
                    head.current_time_code = head.start_time_code;
                }
                head.playing = true;
            }
            PlaybackCommandKind::Pause => head.playing = false,
            PlaybackCommandKind::Stop => head.stop(),
            PlaybackCommandKind::Seek => head.scrub_to(f64::from(cmd.value)),
            PlaybackCommandKind::SetSpeed => head.speed = f64::from(cmd.value),
            PlaybackCommandKind::SetLooping => head.looping = cmd.value != 0.0,
        }
    }
}

/// Transforms an axis-aligned bounding box from USD world space to render world
/// space by applying `basis` to all 8 corners and returning the enclosing
/// axis-aligned bounding box in render space.
///
/// This handles both uniform scaling (MPU correction) and rotation (Z-up →
/// Y-up), which can change which axis the box extents align with.
fn transform_aabb(usd_min: [f32; 3], usd_max: [f32; 3], basis: Mat4) -> ([f32; 3], [f32; 3]) {
    let corners = [
        [usd_min[0], usd_min[1], usd_min[2]],
        [usd_max[0], usd_min[1], usd_min[2]],
        [usd_min[0], usd_max[1], usd_min[2]],
        [usd_max[0], usd_max[1], usd_min[2]],
        [usd_min[0], usd_min[1], usd_max[2]],
        [usd_max[0], usd_min[1], usd_max[2]],
        [usd_min[0], usd_max[1], usd_max[2]],
        [usd_max[0], usd_max[1], usd_max[2]],
    ];
    let mut out_min = [f32::INFINITY; 3];
    let mut out_max = [f32::NEG_INFINITY; 3];
    for c in &corners {
        let p = basis.transform_point3(Vec3::from(*c));
        for i in 0..3 {
            out_min[i] = out_min[i].min(p[i]);
            out_max[i] = out_max[i].max(p[i]);
        }
    }
    (out_min, out_max)
}

/// Resolve an `asset`-typed behavior attribute (`attr`, e.g. `idtx:behavior:module` or
/// `idtx:behavior:manifest`) on the composed prim at `prim_path` against its
/// authoring layer and read the bytes through USD's resolver.
///
/// Returns `None` when the prim is absent, the attribute is unauthored or does
/// not resolve, or the bytes cannot be read — the caller then falls back to a
/// directory-relative reader. Package-internal (`foo.usdz[inner.wasm]`) paths
/// are served transparently by [`StageService::read_asset`].
fn read_asset_via_resolver(service: &StageService, prim_path: &str, attr: &str) -> Option<Vec<u8>> {
    let prim = service.prim_at_path(prim_path)?;
    let resolved = service.resolve_prim_asset(prim, attr).ok().flatten()?;
    match service.read_asset(&resolved) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            tracing::warn!("failed to read behavior asset {resolved} for {prim_path}: {e}");
            None
        }
    }
}

/// Strip surrounding quotes from a stringified `VtValue` repr (USD serializes a
/// `string`/`token` scalar as `"value"`).
fn unquote_repr(repr: &str) -> String {
    repr.trim()
        .trim_start_matches('"')
        .trim_end_matches('"')
        .to_owned()
}

/// Read a `token`/`string` scalar off `prim` at the default time, or `None` when
/// absent/empty.
fn read_text_attr(service: &StageService, prim: idtx_usd::PrimId, attr: &str) -> Option<String> {
    let v = service.eval_attribute(prim, attr, 0.0, true).ok().flatten()?;
    let s = unquote_repr(&v.value_repr);
    (!s.is_empty()).then_some(s)
}

/// Read a `bool` scalar off `prim` (`0`/`1`/`true`/`false`), or `None` when absent.
fn read_bool_attr(service: &StageService, prim: idtx_usd::PrimId, attr: &str) -> Option<bool> {
    let v = service.eval_attribute(prim, attr, 0.0, true).ok().flatten()?;
    match v.value_repr.trim() {
        "1" | "true" | "True" => Some(true),
        "0" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// Read an `int` scalar off `prim`, or `None` when absent/unparseable.
fn read_int_attr(service: &StageService, prim: idtx_usd::PrimId, attr: &str) -> Option<i64> {
    let v = service.eval_attribute(prim, attr, 0.0, true).ok().flatten()?;
    v.value_repr.trim().parse::<i64>().ok()
}

/// Build a [`BehaviorManifest`] from the `IdtxBehaviorAPI` schema attributes
/// authored on the prim at `prim_path` — the single source of truth for a
/// behavior's host manifest (there is no sidecar `idtx.toml` and no fallback).
///
/// The runtime consumes only `capabilities` (linker gate) and `inputs` (action
/// id table) at attach; `kind`/`uuid`/`name`/`version`/`ship`/`memoryMb` round
/// out the manifest. Param *declarations* are intentionally omitted (not
/// consumed at attach — per-prim param *values* ride on the `BehaviorRef`).
///
/// Returns `None` when the prim cannot be resolved (e.g. an embedded behavior
/// with no authored prim), which the attach loop treats as "skip".
fn manifest_from_schema(service: &StageService, prim_path: &str) -> Option<BehaviorManifest> {
    use idtx_behavior::{Capabilities, InputDecl};
    use idtx_scene::behavior::attr;

    if prim_path.is_empty() {
        return None;
    }
    let prim = service.prim_at_path(prim_path)?;

    let kind = read_text_attr(service, prim, attr::KIND)
        .map_or(BehaviorKind::Runtime, |t| BehaviorKind::from_token(&t));
    let uuid = read_text_attr(service, prim, attr::UUID).unwrap_or_default();
    let name = read_text_attr(service, prim, attr::NAME).unwrap_or_default();
    let version = read_text_attr(service, prim, attr::VERSION).unwrap_or_default();
    let ship = read_bool_attr(service, prim, attr::SHIP).unwrap_or(false);

    let capabilities = Capabilities::from_tokens(
        service
            .eval_token_array(prim, attr::CAPABILITIES, 0.0, true)
            .unwrap_or_default(),
    );
    let inputs: Vec<InputDecl> = service
        .eval_token_array(prim, attr::INPUTS, 0.0, true)
        .unwrap_or_default()
        .iter()
        .filter_map(|tok| InputDecl::decode(tok))
        .collect();
    let memory_mb = read_int_attr(service, prim, attr::MEMORY_MB)
        .and_then(|v| u32::try_from(v.max(0)).ok())
        .filter(|&mb| mb > 0);

    Some(BehaviorManifest::from_parts(
        name,
        version,
        uuid,
        kind,
        ship,
        capabilities,
        inputs,
        memory_mb,
    ))
}

/// Find the **local player's** camera entity + authored path, or `None` when the
/// stage has no camera under an explicitly authored `local` player role.
///
/// A camera qualifies when it has an authored [`PlayerRole`] ancestor whose role
/// is [`NetRole::Local`], via `nearest_authored_player_role` — a plain scene
/// camera with no player role does **not** qualify. Among qualifying cameras the
/// pawn camera ([`PLAYER_CAMERA_PATH`][xr_controller_prefab::PLAYER_CAMERA_PATH])
/// is preferred, else the first in query order.
///
/// The single source of truth for "which camera is the local player's", shared
/// by the runtime desktop path ([`StudioSession::local_player_camera_path`]) and
/// the XR-preview camera resolution, so both render through the same camera. Kept
/// ungated (the runtime needs it without the `xr-preview` feature).
pub(crate) fn local_player_camera(
    world: &mut idtx_scene::bevy_ecs::world::World,
) -> Option<(idtx_scene::bevy_ecs::prelude::Entity, String)> {
    use idtx_scene::bevy_ecs::prelude::Entity;
    use idtx_scene::{CameraComp, NetRole, nearest_authored_player_role};

    let mut query = world.query::<(Entity, &PrimNode, &CameraComp)>();
    let candidates: Vec<(Entity, String)> = query
        .iter(world)
        .map(|(entity, node, _)| (entity, node.path.clone()))
        .collect();

    let local: Vec<(Entity, String)> = candidates
        .into_iter()
        .filter(|(entity, _)| nearest_authored_player_role(world, *entity) == Some(NetRole::Local))
        .collect();

    local
        .iter()
        .find(|(_, p)| xr_controller_prefab::is_pawn_camera_path(p))
        .or_else(|| local.first())
        .cloned()
}

/// The local-player pawn root's USD-world transform: the `WorldTransform.matrix`
/// of the prim at [`PLAYER_ROOT_PATH`][xr_controller_prefab::PLAYER_ROOT_PATH]
/// whose nearest authored `idtx:playerRole` is [`NetRole::Local`], or `None` when
/// no such pawn root exists.
///
/// The ECS half of [`StudioSession::local_player_root_matrix`] (which composes it
/// with `projection_basis`), split out so it unit-tests over a bare `World`.
/// Collect-then-filter mirrors [`local_player_camera`] because
/// `nearest_authored_player_role` re-borrows the world and cannot run inside the
/// live query iterator. Gated with the preview seam (`xr-preview`) because both
/// the eye-render path (`local_player_root_matrix`, `idtx_xr`) and the hand-mesh
/// placement (`inject_xr_hand_meshes`) use it; the split still lets it unit-test
/// over a bare `World` (no GPU).
#[cfg(feature = "xr-preview")]
pub(crate) fn local_player_root_world(
    world: &mut idtx_scene::bevy_ecs::world::World,
) -> Option<Mat4> {
    use idtx_scene::bevy_ecs::prelude::Entity;
    use idtx_scene::{NetRole, nearest_authored_player_role};

    let mut query = world.query::<(Entity, &PrimNode, &WorldTransform)>();
    let candidates: Vec<(Entity, Mat4)> = query
        .iter(world)
        .filter(|(_, node, _)| node.path == xr_controller_prefab::PLAYER_ROOT_PATH)
        .map(|(entity, _, xform)| (entity, xform.matrix))
        .collect();
    candidates
        .into_iter()
        .find(|(entity, _)| nearest_authored_player_role(world, *entity) == Some(NetRole::Local))
        .map(|(_, m)| m)
}

/// Computes the scene-wide world-space AABB in USD coordinates by unioning all
/// [`BoundsComp`] entries that are visible and of default render purpose.
///
/// Returns `None` when the stage has no bounded geometry.
/// The result is in **USD world space** — apply [`transform_aabb`] with the
/// stage basis before passing to the orbit camera.
fn compute_scene_aabb_from_ecs(hydrator: &SceneHydrator) -> Option<([f32; 3], [f32; 3])> {
    let world = hydrator.world();
    let mut union_min = [f32::INFINITY; 3];
    let mut union_max = [f32::NEG_INFINITY; 3];
    let mut found = false;

    for entity_ref in world.iter_entities() {
        let Some(bounds) = entity_ref.get::<BoundsComp>() else {
            continue;
        };
        if let Some(vis) = entity_ref.get::<VisibilityComp>() {
            if !vis.visible {
                continue;
            }
            if vis.purpose != RenderPurpose::Default {
                continue;
            }
        }
        for i in 0..3 {
            union_min[i] = union_min[i].min(bounds.minimum[i]);
            union_max[i] = union_max[i].max(bounds.maximum[i]);
        }
        found = true;
    }

    found.then_some((union_min, union_max))
}

/// Returns 8 OBB corners in render world space for the selected prim, or `None`
/// if the prim has no bounds.
///
/// For prims with their own [`BoundsComp`], the local-space extents are
/// transformed through the prim's [`WorldTransform`] matrix and then through
/// `projection_basis` to produce a properly oriented bounding box (OBB).
///
/// For container prims with no `BoundsComp` (packed stage roots, etc.) we fall
/// back to unioning descendant world-space AABBs and returning those as
/// axis-aligned OBB corners.
/// Push the 12 wireframe edges of an 8-corner OBB (from
/// [`selection_obb_for_path`]) as coloured overlay lines.
fn push_obb_box_lines(
    lines: &mut Vec<idtx_render::OverlayLine>,
    corners: &[[f32; 3]; 8],
    color: [f32; 4],
) {
    // Corner order (see `selection_obb_for_path`): bit0=x, bit1=y, bit2=z.
    const EDGES: [(usize, usize); 12] = [
        (0, 1), (1, 3), (3, 2), (2, 0), // z = min face
        (4, 5), (5, 7), (7, 6), (6, 4), // z = max face
        (0, 4), (1, 5), (2, 6), (3, 7), // verticals
    ];
    for (a, b) in EDGES {
        lines.push(idtx_render::OverlayLine {
            a: corners[a],
            b: corners[b],
            color,
        });
    }
}

/// Visual style a behavior requested for a highlighted prim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HighlightStyle {
    /// Coloured wireframe box around the prim (the low-risk fallback cue).
    Box,
    /// Surface tint overlay; the colour's alpha channel is the tint strength.
    Tint,
    /// Silhouette outline around the prim.
    Outline,
}

/// One behavior-requested highlight resolved to a USD path for this frame.
#[derive(Clone, Debug, PartialEq)]
struct HighlightEntry {
    /// USD prim path the highlight targets.
    path: String,
    /// Linear-RGBA colour (alpha = tint strength for [`HighlightStyle::Tint`]).
    color: [f32; 4],
    /// How to render the highlight.
    style: HighlightStyle,
    /// Outline band width in pixels ([`HighlightStyle::Outline`] only; `0.0` =
    /// renderer default). Ignored for box / tint.
    thickness: f32,
}

/// Resolve the behavior-posted overlay commands for this frame into styled
/// highlights (USD path + colour + style). `Highlight`→box, `HighlightTint`→
/// tint, `HighlightOutline`→outline; other ops (material override,
/// post-process) are not turned into highlights here.
///
/// Highlights render by **geometry** path. When a command targets a gprim the
/// result is a single entry for that prim; when it targets an `Xform`/`Scope`/…
/// (which carries no geometry of its own) the target is expanded into its leaf
/// gprims via [`leaf_gprim_entities`] — the same subtree traversal a convex hull
/// uses — and one entry is emitted per leaf, so a behavior that highlights a
/// grouping prim lights up all the geometry beneath it.

/// Build host-drawn hit-point cue rings for this frame's widget raycast hits
/// (ABI 3). Each cue is a small ring in the panel plane (oriented by the hit
/// normal) plus a short normal stub, so the user sees exactly where a pointer
/// crosses a widget — drawn by the host, independent of behavior capabilities.
/// Build the light-blue ring cues for this frame's widget raycast hits.
///
/// The markers (and the overlay-line pass that draws them) live in **render-world
/// space**, whose unit is `render_metres_per_unit` real metres — the
/// [`RenderWorldUnitScale`](crate::RenderWorldUnitScale) the stage was opened
/// with (the stage `metersPerUnit` is already folded into the projection basis
/// that produced these positions). A ring radius expressed as a literal in
/// render-world units would therefore be 12 mm only on a 1-unit-per-metre stage
/// and shrink to a fraction of a millimetre on, say, a centimetre-unit stage —
/// the "tiny marker" bug. To keep the cue a constant *physical* size regardless
/// of the stage's unit scale, convert the metric radius into render-world units:
/// `radius_units = RING_RADIUS_M / render_metres_per_unit`.
fn widget_marker_lines(
    markers: &[idtx_behavior::WidgetMarker],
    render_metres_per_unit: f64,
) -> Vec<idtx_render::OverlayLine> {
    const RING_SEGMENTS: usize = 20;
    /// Target physical radius of the cue ring, in real metres.
    const RING_RADIUS_M: f32 = 0.012;
    // Light-blue cue, matching the editor's interaction accent.
    const CUE_COLOR: [f32; 4] = [0.45, 0.72, 1.0, 1.0];
    // Metric radius → render-world units. Guard against a zero/non-finite scale
    // (a neutral 1 unit = 1 m keeps the cue visible rather than collapsing it).
    #[allow(clippy::cast_possible_truncation)]
    let mpu = if render_metres_per_unit.is_finite() && render_metres_per_unit > 0.0 {
        render_metres_per_unit as f32
    } else {
        1.0
    };
    let ring_radius = RING_RADIUS_M / mpu;
    let mut out = Vec::new();
    for m in markers {
        let n = if m.normal.length_squared() > 1e-8 {
            m.normal.normalize()
        } else {
            glam::Vec3::Z
        };
        // Two in-plane tangents perpendicular to the normal.
        let seed = if n.x.abs() < 0.9 { glam::Vec3::X } else { glam::Vec3::Y };
        let u = n.cross(seed).normalize_or_zero() * ring_radius;
        let v = n.cross(u).normalize_or_zero() * ring_radius;
        let mut ring: Vec<[f32; 3]> = Vec::with_capacity(RING_SEGMENTS + 1);
        for i in 0..=RING_SEGMENTS {
            #[allow(clippy::cast_precision_loss)]
            let a = (i as f32 / RING_SEGMENTS as f32) * std::f32::consts::TAU;
            let p = m.position + u * a.cos() + v * a.sin();
            ring.push(p.to_array());
        }
        idtx_render::push_polyline(&mut out, &ring, CUE_COLOR);
    }
    out
}

/// Assemble this frame's behavior-driven overlay lines: the world-space
/// `draw_line` / `draw_arc` strokes (teleport arc + landing marker) plus the
/// ABI-3 widget hit-point cue rings. Both the inline `tick_behaviors` path and
/// the externally-owned runtime path (`resolve_overlay_highlights_external`)
/// funnel through here so the two can never diverge — the visionOS gaze-cue bug
/// was precisely such a divergence, where only the inline path appended the
/// widget rings and the headset path dropped them.
fn assemble_behavior_overlay_lines(
    overlay_commands: &[idtx_behavior::OverlayCommand],
    widget_markers: &[idtx_behavior::WidgetMarker],
    render_metres_per_unit: f64,
) -> Vec<idtx_render::OverlayLine> {
    let mut lines = resolve_overlay_lines(overlay_commands);
    lines.extend(widget_marker_lines(widget_markers, render_metres_per_unit));
    lines
}

/// Resolve the transient `draw_line` / `draw_arc` overlay commands a behavior
/// posted this frame into render-world overlay lines (world/stage space, which
/// equals render world space). Pure — no hydrator needed, since the geometry is
/// already in world coordinates. `Arc` is sampled against a flat `y = 0` ground
/// (a reusable primitive for simple scenes); the teleport behavior instead emits
/// its exact sampled trajectory as `Line` segments, so its visual matches the
/// real-geometry landing regardless of floor height.
///
/// The authored world-space `width` is honored: a command with a meaningful width
/// (≥ 1 mm) is lowered onto a thick [`idtx_render::push_ribbon`] stroke; a
/// near-zero width stays a crisp 1-px line.
fn resolve_overlay_lines(commands: &[idtx_behavior::OverlayCommand]) -> Vec<idtx_render::OverlayLine> {
    use idtx_behavior::OverlayOp;
    // World up: the plane a thick stroke's width spreads in (see `push_ribbon`).
    const RIBBON_UP: [f32; 3] = [0.0, 1.0, 0.0];
    // Below ~1 mm a world-space ribbon is sub-pixel at any useful distance, so a
    // crisp 1-px line reads better than an aliased sliver — fall back to a line.
    const MIN_RIBBON_WIDTH_M: f32 = 1.0e-3;
    let mut out = Vec::new();
    for cmd in commands {
        match &cmd.op {
            OverlayOp::Line {
                start, end, color, width,
            } => {
                // Honor the authored world-space width: a meaningful width renders a
                // thick ribbon stroke; a near-zero width stays a 1-px line.
                if *width > MIN_RIBBON_WIDTH_M {
                    idtx_render::push_ribbon(
                        &mut out,
                        &[start.to_array(), end.to_array()],
                        *width,
                        RIBBON_UP,
                        color.to_array(),
                    );
                } else {
                    idtx_render::push_line(
                        &mut out,
                        start.to_array(),
                        end.to_array(),
                        color.to_array(),
                    );
                }
            }
            OverlayOp::Arc {
                start,
                dir,
                speed,
                gravity,
                color,
                width,
            } => {
                if *width > MIN_RIBBON_WIDTH_M {
                    let points = idtx_render::sample_arc(
                        start.to_array(),
                        dir.to_array(),
                        *speed,
                        *gravity,
                        0.0,
                        1.0 / 30.0,
                        64,
                    );
                    idtx_render::push_ribbon(
                        &mut out,
                        &points,
                        *width,
                        RIBBON_UP,
                        color.to_array(),
                    );
                } else {
                    idtx_render::push_arc(
                        &mut out,
                        start.to_array(),
                        dir.to_array(),
                        *speed,
                        *gravity,
                        0.0,
                        1.0 / 30.0,
                        64,
                        color.to_array(),
                    );
                }
            }
            _ => {}
        }
    }
    out
}

fn resolve_overlay_highlights(
    hydrator: &SceneHydrator,
    commands: &[idtx_behavior::OverlayCommand],
) -> Vec<HighlightEntry> {
    use idtx_behavior::OverlayOp;
    let world = hydrator.world();
    let mut out = Vec::new();
    for cmd in commands {
        let (color, style, thickness) = match cmd.op {
            OverlayOp::Highlight { color } => (color, HighlightStyle::Box, 0.0),
            OverlayOp::HighlightTint { color } => (color, HighlightStyle::Tint, 0.0),
            OverlayOp::HighlightOutline { color, thickness_px } => {
                (color, HighlightStyle::Outline, thickness_px)
            }
            _ => continue,
        };
        let Some(target) = bevy_ecs::entity::Entity::try_from_bits(cmd.target.to_bits()) else {
            continue;
        };
        let Some(node) = world.get::<idtx_scene::PrimNode>(target) else {
            continue;
        };
        // Expand the target into the leaf gprims to highlight (just the prim
        // itself when it is already a gprim), emitting one entry per leaf.
        for leaf in leaf_gprim_entities(hydrator, &node.path) {
            if let Some(leaf_node) = world.get::<idtx_scene::PrimNode>(leaf) {
                out.push(HighlightEntry {
                    path: leaf_node.path.clone(),
                    color: color.to_array(),
                    style,
                    thickness,
                });
            }
        }
    }
    out
}

/// Split resolved highlights by style into the three render channels: box
/// (path, colour) list, tint map (path → colour), and outline (path, colour,
/// `thickness_px`) list. Pure — unit-tested without a hydrator. A later style for
/// the same path does not clobber an earlier one in a different channel; within
/// the tint map, last-writer-wins per path.
fn partition_highlights(
    entries: &[HighlightEntry],
) -> (
    Vec<(String, [f32; 4])>,
    std::collections::HashMap<String, [f32; 4]>,
    Vec<(String, [f32; 4], f32)>,
) {
    let mut boxes = Vec::new();
    let mut tints = std::collections::HashMap::new();
    let mut outlines = Vec::new();
    for e in entries {
        match e.style {
            HighlightStyle::Box => boxes.push((e.path.clone(), e.color)),
            HighlightStyle::Tint => {
                tints.insert(e.path.clone(), e.color);
            }
            HighlightStyle::Outline => outlines.push((e.path.clone(), e.color, e.thickness)),
        }
    }
    (boxes, tints, outlines)
}

/// Collect [`HudElement`]s from every ECS entity carrying a [`HudComp`] (the
/// `IdtxHud` schema). Produces the transient per-frame
/// [`RenderWorldSnapshot::hud_elements`] the HUD pass consumes. Elements with an
/// empty image path or non-positive opacity are skipped (the renderer would
/// no-op on them anyway). Order follows ECS iteration — HUD elements composite
/// independently, so order only affects overlap, which the crosshair case does
/// not hit.
fn collect_hud_elements(world: &idtx_scene::bevy_ecs::world::World) -> Vec<HudElement> {
    let mut out = Vec::new();
    for entity_ref in world.iter_entities() {
        let Some(hud) = entity_ref.get::<HudComp>() else {
            continue;
        };
        if hud.image.is_empty() || hud.opacity <= 0.0 {
            continue;
        }
        out.push(HudElement {
            image_path: hud.image.clone(),
            size_px: hud.size_px,
            anchor: hud.anchor,
            opacity: hud.opacity,
        });
    }
    out
}

fn selection_obb_for_path(
    hydrator: &SceneHydrator,
    path: &str,
    projection_basis: Mat4,
) -> Option<[[f32; 3]; 8]> {
    let entity = hydrator.entity_for_path(path)?;
    let world = hydrator.world();

    // Fast path: entity has its own local-space extents and world transform.
    if let Some(bounds) = world.get::<BoundsComp>(entity) {
        let lo = bounds.local_minimum;
        let hi = bounds.local_maximum;

        let world_xform = world
            .get::<WorldTransform>(entity)
            .map_or(Mat4::IDENTITY, |wt| wt.matrix);

        // Combined: local → USD world → render world
        let to_render = projection_basis * world_xform;

        let local_corners: [[f32; 3]; 8] = [
            [lo[0], lo[1], lo[2]],
            [hi[0], lo[1], lo[2]],
            [lo[0], hi[1], lo[2]],
            [hi[0], hi[1], lo[2]],
            [lo[0], lo[1], hi[2]],
            [hi[0], lo[1], hi[2]],
            [lo[0], hi[1], hi[2]],
            [hi[0], hi[1], hi[2]],
        ];
        let mut corners = [[0f32; 3]; 8];
        for (i, c) in local_corners.iter().enumerate() {
            let p = to_render.transform_point3(Vec3::from(*c));
            corners[i] = p.into();
        }
        return Some(corners);
    }

    // Fallback for packed stage roots and other container prims: union all
    // descendant world-space AABBs, then return as AABB-aligned OBB corners.
    let prefix = format!("{path}/");
    let mut union_min = [f32::INFINITY; 3];
    let mut union_max = [f32::NEG_INFINITY; 3];
    let mut found = false;
    for entity_ref in world.iter_entities() {
        let (Some(bounds), Some(node)) =
            (entity_ref.get::<BoundsComp>(), entity_ref.get::<PrimNode>())
        else {
            continue;
        };
        if node.path.starts_with(&prefix) {
            for i in 0..3 {
                union_min[i] = union_min[i].min(bounds.minimum[i]);
                union_max[i] = union_max[i].max(bounds.maximum[i]);
            }
            found = true;
        }
    }
    if !found {
        return None;
    }
    let (rmin, rmax) = transform_aabb(union_min, union_max, projection_basis);
    let lo = rmin;
    let hi = rmax;
    Some([
        [lo[0], lo[1], lo[2]],
        [hi[0], lo[1], lo[2]],
        [lo[0], hi[1], lo[2]],
        [hi[0], hi[1], lo[2]],
        [lo[0], lo[1], hi[2]],
        [hi[0], lo[1], hi[2]],
        [lo[0], hi[1], hi[2]],
        [hi[0], hi[1], hi[2]],
    ])
}

/// Resolves a picked entity to the correct tree-selection path, respecting
/// packed-stage expansion state.
///
/// Walks from `hit_entity` up through `PrimNode.parent` links.  At each step,
/// if the entity has a [`PackedStageMetadata`] component AND its path hash is
/// NOT in `packed_revealed`, that path is remembered as a candidate.  The
/// *outermost* such candidate (closest to the scene root) is returned, because
/// if the outer packed stage is not expanded its children are invisible in the
/// tree regardless of inner state.  If no unexpanded packed ancestor is found,
/// the original hit path is returned.
fn resolve_pick_path(
    hydrator: &SceneHydrator,
    hit_entity: idtx_scene::bevy_ecs::entity::Entity,
    packed_revealed: &HashSet<u64>,
) -> Option<String> {
    let world = hydrator.world();
    let hit_path = world.get::<PrimNode>(hit_entity)?.path.clone();

    let mut outermost_unexpanded: Option<String> = None;
    let mut current = Some(hit_entity);

    while let Some(entity) = current {
        let Some(node) = world.get::<PrimNode>(entity) else { break; };
           
        if world.get::<PackedStageMetadata>(entity).is_some() {
            let hash = stable_prim_hash(&node.path);
            if !packed_revealed.contains(&hash) {
                outermost_unexpanded = Some(node.path.clone());
            }
        }

        current = node.parent.and_then(|pid| hydrator.entity_for(pid));
    }

    Some(outermost_unexpanded.unwrap_or(hit_path))
}

// ── Collaboration helpers ─────────────────────────────────────────────────────

/// Pure classifier/extractor behind [`StudioSession::authored_trs_for_outbound`].
///
/// Returns the exact `(translation, rotation_xyz_degrees, scale)` when `ops` is a
/// pure translate/`rotateXYZ`/scale stack (identity defaults for absent ops), or
/// `None` for any op the inspector treats as non-exact: a non-editable stack,
/// inverse ops, pivots, animated-only ops (no authored default), a matrix op, or
/// a non-`rotateXYZ` rotate / `orient` (which arrive as
/// [`idtx_usd::XformOpType::Unsupported`]).
///
/// Mirrors `classify_authored_transform` in `idtx-studio-shell`'s projection so
/// a prim the inspector shows as exact TRS is exactly the set the collab path
/// sends as a decompose-free `SeparateTransform`.
#[cfg(all(feature = "remote", feature = "authoring"))]
fn trs_ops_for_outbound(
    ops: &[idtx_usd::AuthoredXformOp],
    editable: bool,
) -> Option<([f64; 3], [f64; 3], [f64; 3])> {
    use idtx_usd::XformOpType;

    if !editable {
        return None;
    }
    let mut translation = [0.0_f64; 3];
    let mut rotation_xyz_degrees = [0.0_f64; 3];
    let mut scale = [1.0_f64; 3];
    for op in ops {
        if op.inverse || op.name.contains(":pivot") || !op.has_default {
            return None;
        }
        match op.op_type {
            XformOpType::Translate => translation = op.value,
            XformOpType::RotateXyz => rotation_xyz_degrees = op.value,
            XformOpType::Scale => scale = op.value,
            XformOpType::Unsupported => return None,
        }
    }
    Some((translation, rotation_xyz_degrees, scale))
}

/// Build a flat row-major USD **world** matrix from a server-sent
/// [`RemoteWorldTransform`][idtx_collab::client::RemoteWorldTransform].
///
/// The matrix variant is already a flat row-major USD matrix (converted from the
/// wire convention by the collab client). TRS uses the shared USD-canonical
/// constructor; both forms reject invalid data before stage authoring.
#[cfg(all(feature = "remote", feature = "authoring"))]
fn remote_world_to_usd_mat(
    world: &idtx_collab::client::RemoteWorldTransform,
) -> Result<idtx_collab::conv::UsdMat4, StageError> {
    use idtx_collab::client::RemoteWorldTransform;
    match world {
        RemoteWorldTransform::Matrix(m) => {
            idtx_usd::validate_usd_affine(m)?;
            Ok(*m)
        }
        RemoteWorldTransform::Separate {
            translation,
            rotation_deg,
            scale,
        } => idtx_usd::usd_trs_matrix(idtx_usd::TransformValues {
            translation: *translation, rotation_xyz_degrees: *rotation_deg, scale: *scale,
        }),
    }
}

/// Returns `path` together with every proper ancestor prim path, leaf first and
/// excluding the pseudo-root `/`.
///
/// `"/Scene/Payload/Detail/Block"` →
/// `["/Scene/Payload/Detail/Block", "/Scene/Payload/Detail", "/Scene/Payload", "/Scene"]`.
///
/// Used by [`StudioSession::create_opinion`] to reproject the whole chain of
/// prims whose root-layer provenance `SdfCreatePrimInLayer` may have just
/// touched (the target and any ancestor that gained a fresh `over`).
#[cfg(feature = "authoring")]
fn ancestor_chain_paths(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = path;
    loop {
        out.push(cur.to_owned());
        match cur.rfind('/') {
            // `Some(0)` is a top-level prim (`/Scene`); its only ancestor is the
            // pseudo-root, which never carries a paintable spec. `None` is a
            // malformed relative path — stop rather than loop forever.
            Some(0) | None => break,
            Some(idx) => cur = &cur[..idx],
        }
    }
    out
}

/// Derive a USD-legal prim name from a project-relative asset path, used to
/// name the child prim created when a stage asset is dropped onto a scene-tree
/// prim ([`StudioSession::add_asset_child_arc`]).
///
/// Takes the file stem (last path component, minus a single trailing
/// extension) and sanitises it to a USD identifier: names must be non-empty,
/// contain only ASCII letters, digits and underscores, and must not begin with
/// a digit. Invalid characters become `_`, a leading digit is prefixed with
/// `_`, and an empty stem falls back to `Asset`.
#[must_use]
fn prim_name_from_asset(asset_path: &str) -> String {
    // Last path component, tolerating both `/` and `\` separators.
    let file = asset_path.rsplit(['/', '\\']).next().unwrap_or(asset_path);
    // Drop a single trailing extension (keep any earlier dots as part of stem).
    let stem = file.rsplit_once('.').map_or(file, |(base, _ext)| base);
    let mut name = String::with_capacity(stem.len());
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            name.push(ch);
        } else {
            name.push('_');
        }
    }
    if name.is_empty() {
        return "Asset".to_owned();
    }
    if name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        name.insert(0, '_');
    }
    name
}

/// Read a controller asset's own coordinate settings (`upAxis` / `metersPerUnit`)
/// so the packaged runtime can normalize the referenced mesh into world (metre,
/// Y-up) space exactly as the editor preview does.
///
/// Opens `asset` topology-only (no payloads) for its metadata. Falls back to
/// [`CoordinateSettings::WORLD`] (identity — no correction) when the asset cannot
/// be opened, matching the preview path's fail-safe.
///
/// The twin of `idtx-studio-shell::preview_assembly::read_controller_settings`;
/// exposed here so the packaged XR runtime (`idtx-runtime-core`), which has no
/// direct `idtx-usd` dependency, can build the same
/// [`xr_controller_prefab::controller_model_over_layer`] injection.
#[must_use]
pub fn read_controller_coordinate_settings(
    asset: &Path,
) -> xr_controller_prefab::CoordinateSettings {
    match StageService::open_topology_only(asset) {
        Ok(stage) => {
            let meta = stage.metadata();
            xr_controller_prefab::CoordinateSettings::from_usd(&meta.up_axis, meta.meters_per_unit)
        }
        Err(e) => {
            tracing::warn!(
                "controller asset {} did not open for metadata ({e}); placing model without \
                 up-axis/units correction",
                asset.display()
            );
            xr_controller_prefab::CoordinateSettings::WORLD
        }
    }
}

/// Pure resolution of the per-frame shadow settings from the three inputs, split
/// out from [`StudioSession::resolved_shadow_settings`] so the full decision
/// matrix can be unit-tested without constructing a session (no USD link).
///
/// - `!capability` (product forbids shadows: Inspector/Poser) → always `Off`.
/// - The authored `mode` picks the desired tier 1:1: `Off`→`Off`,
///   `Hard`→`Hard` (true single-tap), `Smooth`→`Smooth` (3×3 PCF),
///   `Soft`→`Soft` (PCSS).
/// - The hardware `cap` clamps that tier *downward* (`ShadowTier` is `Ord`ered
///   `Off < Hard < Smooth < Soft`), so a Quest cap of `Smooth` turns an authored
///   `Soft` into `Smooth`, and a cap of `Off` disables shadows regardless of the
///   authored mode.
#[must_use]
pub(crate) fn resolve_shadow_settings(
    mode: idtx_usd::ShadowMode,
    capability: bool,
    cap: idtx_render::ShadowTier,
) -> idtx_render::RenderShadowSettings {
    use idtx_render::{RenderShadowSettings, ShadowTier};
    if !capability {
        return RenderShadowSettings::OFF;
    }
    let authored = match mode {
        idtx_usd::ShadowMode::Off => ShadowTier::Off,
        idtx_usd::ShadowMode::Hard => ShadowTier::Hard,
        idtx_usd::ShadowMode::Smooth => ShadowTier::Smooth,
        idtx_usd::ShadowMode::Soft => ShadowTier::Soft,
    };
    RenderShadowSettings::new(authored.min(cap))
}

/// Walk the subtree under `root_path` and return the ECS entities of its **leaf
/// gprims** — the renderable geometry to act on when a path denotes a whole
/// subtree rather than a single piece of geometry. This is the one traversal
/// shared by the collision-hull mesh gather
/// ([`StudioSession::gather_collision_mesh_points`]) and the behavior highlight
/// resolve ([`resolve_overlay_highlights`]), so highlighting an `Xform` lights
/// up exactly the gprims a convex hull would be built from.
///
/// Mirrors `OpenUSD`'s "do not nest gprims under gprims" rule:
/// - If `root_path` is itself a gprim, returns just that entity (a gprim is a
///   leaf — never descended into), with no subtree scan.
/// - Otherwise (an `Xform`/`Scope`/…) the subtree is walked and every gprim
///   whose nearest gprim ancestor within the subtree is itself is collected —
///   the first gprim on each branch; gprims nested under another gprim are
///   skipped.
///
/// Returns empty when `root_path` is unknown. Order is unspecified (the walk
/// iterates a hash index).
fn leaf_gprim_entities(
    hydrator: &SceneHydrator,
    root_path: &str,
) -> Vec<bevy_ecs::entity::Entity> {
    let world = hydrator.world();
    let Some(root_entity) = hydrator.entity_for_path(root_path) else {
        return Vec::new();
    };

    // Fast path: the root is itself a gprim → it *is* the leaf; no subtree scan.
    if world
        .get::<idtx_scene::PrimNode>(root_entity)
        .is_some_and(|n| is_gprim_type(&n.type_name))
    {
        return vec![root_entity];
    }

    // Non-gprim root: index the subtree path → (entity, is_gprim), then pick the
    // first gprim on each branch.
    let subtree = hydrator.subtree_entities(root_path);
    let mut index: std::collections::HashMap<String, (bevy_ecs::entity::Entity, bool)> =
        std::collections::HashMap::with_capacity(subtree.len());
    for &entity in &subtree {
        if let Some(node) = world.get::<idtx_scene::PrimNode>(entity) {
            index.insert(node.path.clone(), (entity, is_gprim_type(&node.type_name)));
        }
    }
    leaf_gprims_from_index(root_path, &index)
}

/// Whether `candidate` lies within the subtree rooted at `root` — the root
/// itself or any descendant. Mirrors the path-prefix reachability of
/// [`idtx_scene::Hydrator::despawn_subtree`] (a `root/` prefix, so `/A` does
/// not match `/AB`), used to decide whether a pruned subtree swallowed the
/// current selection.
#[cfg(all(feature = "remote", feature = "authoring"))]
fn path_in_subtree(candidate: &str, root: &str) -> bool {
    candidate == root || candidate.strip_prefix(root).is_some_and(|rest| rest.starts_with('/'))
}

/// Pure leaf-gprim selection over a pre-built subtree `index` (path →
/// `(entity, is_gprim)`): every gprim other than `root_path` with no gprim
/// ancestor strictly between it and the root. Split out from
/// [`leaf_gprim_entities`] so it is unit-testable against a hand-built index
/// without a hydrator. Assumes a non-gprim root (the gprim-root fast path is
/// handled by the caller before the index is built).
fn leaf_gprims_from_index(
    root_path: &str,
    index: &std::collections::HashMap<String, (bevy_ecs::entity::Entity, bool)>,
) -> Vec<bevy_ecs::entity::Entity> {
    index
        .iter()
        .filter(|(path, (_, is_gprim))| *is_gprim && path.as_str() != root_path)
        .filter(|(path, _)| !gprim_ancestor_between(path, root_path, index))
        .map(|(_, (entity, _))| *entity)
        .collect()
}

/// Whether `type_name` is a USD gprim (imageable *leaf* geometry). Per the
/// `OpenUSD` rule "do not nest gprims under gprims", the shared subtree walk
/// [`leaf_gprim_entities`] (collision-hull mesh gather + behavior highlight
/// resolve) prunes its traversal at any prim of one of these types.
fn is_gprim_type(type_name: &str) -> bool {
    matches!(
        type_name,
        "Mesh"
            | "Sphere"
            | "Cube"
            | "Cylinder"
            | "Cone"
            | "Capsule"
            | "Plane"
            | "Points"
            | "BasisCurves"
            | "NurbsCurves"
            | "NurbsPatch"
            | "HermiteCurves"
            | "TetMesh"
    )
}

/// Whether a gprim lies strictly between `child_path` and `root_path` in
/// namespace — i.e. walking up `child_path`'s ancestors (excluding `root_path`
/// itself) reaches a path that `index` marks as a gprim. Used by the shared
/// [`leaf_gprim_entities`] walk to prune at the first gprim on each branch: a
/// gprim with such an ancestor is nested under another gprim and must be skipped.
///
/// `index` maps prim path → `(entity, is_gprim)`; only the gprim flag is read.
fn gprim_ancestor_between(
    child_path: &str,
    root_path: &str,
    index: &std::collections::HashMap<String, (bevy_ecs::entity::Entity, bool)>,
) -> bool {
    let mut cursor = child_path;
    while let Some(slash) = cursor.rfind('/') {
        let parent = &cursor[..slash];
        if parent.is_empty() || parent == root_path {
            return false;
        }
        if index.get(parent).is_some_and(|(_, is_gprim)| *is_gprim) {
            return true;
        }
        cursor = parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── local_player_camera fixtures ──────────────────────────────────────
    use idtx_scene::bevy_ecs::prelude::Entity;
    use idtx_scene::bevy_ecs::world::World;
    use idtx_scene::{CameraComp, NetRole, PlayerRole};
    use idtx_usd::{PrimId, PrimState, StageGeneration};

    // ── collision-hull gprim traversal pruning ────────────────────────────────

    // ── widget marker cue sizing ──────────────────────────────────────────────

    /// The hit-cue ring keeps a constant *physical* radius (≈12 mm) regardless of
    /// the render-world unit scale, so a centimetre-unit stage no longer draws a
    /// sub-millimetre marker. A ring vertex's distance from the marker centre is
    /// the radius in render-world units; the physical radius is that × the unit's
    /// metres-per-unit, which must stay ≈`RING_RADIUS_M` across scales.
    #[test]
    fn widget_marker_ring_is_constant_physical_size() {
        let marker = idtx_behavior::WidgetMarker {
            position: glam::Vec3::new(1.0, 2.0, 3.0),
            normal: glam::Vec3::Z,
        };
        // Physical radius recovered from the generated ring at a given unit scale.
        let physical_radius = |mpu: f64| -> f32 {
            let lines = widget_marker_lines(std::slice::from_ref(&marker), mpu);
            assert!(!lines.is_empty(), "expected a ring for one marker");
            let r_units = glam::Vec3::from(lines[0].a).distance(marker.position);
            #[allow(clippy::cast_possible_truncation)]
            {
                r_units * mpu as f32
            }
        };
        let metre = physical_radius(1.0);
        let centimetre = physical_radius(0.01);
        let millimetre = physical_radius(0.001);
        assert!((metre - 0.012).abs() < 1e-6, "metre stage: {metre}");
        assert!((centimetre - 0.012).abs() < 1e-6, "cm stage: {centimetre}");
        assert!((millimetre - 0.012).abs() < 1e-6, "mm stage: {millimetre}");
    }

    /// A zero / non-finite unit scale must not collapse the cue to a point; the
    /// guard falls back to a neutral 1 unit = 1 m so the ring stays visible.
    #[test]
    fn widget_marker_ring_survives_degenerate_scale() {
        let marker = idtx_behavior::WidgetMarker {
            position: glam::Vec3::ZERO,
            normal: glam::Vec3::Z,
        };
        for mpu in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let lines = widget_marker_lines(std::slice::from_ref(&marker), mpu);
            let r_units = glam::Vec3::from(lines[0].a).distance(marker.position);
            assert!((r_units - 0.012).abs() < 1e-6, "mpu={mpu}: r={r_units}");
        }
    }

    /// Regression guard for the visionOS gaze-cue bug: the overlay-line assembly
    /// that both the inline and the externally-owned (headset runtime) paths use
    /// must lower widget hit-point markers into cue-ring lines. Before the fix the
    /// external path built only the `draw_line`/`draw_arc` overlay commands, so the
    /// gaze/pinch cue rendered on desktop but never on the headset. With no markers
    /// and no commands the assembly is empty; a single marker must yield a ring.
    #[test]
    fn behavior_overlay_assembly_lowers_widget_markers_to_rings() {
        let empty = assemble_behavior_overlay_lines(&[], &[], 1.0);
        assert!(empty.is_empty(), "no commands + no markers → no lines");

        let marker = idtx_behavior::WidgetMarker {
            position: glam::Vec3::new(0.0, 1.0, 0.0),
            normal: glam::Vec3::Z,
        };
        let with_marker =
            assemble_behavior_overlay_lines(&[], std::slice::from_ref(&marker), 1.0);
        assert!(
            !with_marker.is_empty(),
            "a widget marker must produce cue-ring overlay lines on every path"
        );
    }

    #[cfg(all(feature = "remote", feature = "authoring"))]
    #[test]
    fn path_in_subtree_matches_root_and_descendants_only() {
        // The pruned subtree root matches itself and genuine descendants.
        assert!(path_in_subtree("/World/A", "/World/A"));
        assert!(path_in_subtree("/World/A/Child", "/World/A"));
        assert!(path_in_subtree("/World/A/Child/Deep", "/World/A"));
        // A sibling with a shared string prefix is NOT in the subtree.
        assert!(!path_in_subtree("/World/AB", "/World/A"));
        assert!(!path_in_subtree("/World/B", "/World/A"));
        // An ancestor of the pruned root is not pruned either.
        assert!(!path_in_subtree("/World", "/World/A"));
    }

    #[test]
    fn is_gprim_type_classifies_leaf_geometry() {
        assert!(is_gprim_type("Mesh"));
        assert!(is_gprim_type("Sphere"));
        assert!(is_gprim_type("BasisCurves"));
        // Grouping / non-geometry prims are not gprims — the walk recurses them.
        assert!(!is_gprim_type("Xform"));
        assert!(!is_gprim_type("Scope"));
        assert!(!is_gprim_type("Material"));
        assert!(!is_gprim_type(""));
    }

    #[test]
    fn gprim_ancestor_between_prunes_nested_gprims() {
        use idtx_scene::bevy_ecs::prelude::Entity;
        // Namespace: /World (Xform) → /World/Grp (Xform) → /World/Grp/Mesh (Mesh)
        //            → /World/Grp/Mesh/Sub (a nested Mesh, invalid per the rule).
        let mut index: std::collections::HashMap<String, (Entity, bool)> =
            std::collections::HashMap::new();
        index.insert("/World".to_owned(), (Entity::from_raw_u32(0).unwrap(), false));
        index.insert("/World/Grp".to_owned(), (Entity::from_raw_u32(1).unwrap(), false));
        index.insert("/World/Grp/Mesh".to_owned(), (Entity::from_raw_u32(2).unwrap(), true));
        index.insert(
            "/World/Grp/Mesh/Sub".to_owned(),
            (Entity::from_raw_u32(3).unwrap(), true),
        );

        // A Mesh reached only through Xforms has no gprim ancestor → collected.
        assert!(!gprim_ancestor_between("/World/Grp/Mesh", "/World", &index));
        // A gprim nested under another gprim has one → pruned (skipped).
        assert!(gprim_ancestor_between("/World/Grp/Mesh/Sub", "/World", &index));
        // Stops at the root: an intermediate gprim beyond the root is not counted.
        assert!(!gprim_ancestor_between("/World/Grp/Mesh/Sub", "/World/Grp/Mesh", &index));
    }

    #[test]
    fn leaf_gprims_from_index_takes_first_gprim_on_each_branch() {
        use idtx_scene::bevy_ecs::prelude::Entity;
        // /World (Xform) root with two branches:
        //   /World/A (Xform) → /World/A/Mesh (Mesh)          ← leaf
        //   /World/B (Mesh)                                   ← leaf
        //                     → /World/B/Nested (Mesh)        ← pruned (under a gprim)
        let e = |i: u32| Entity::from_raw_u32(i).unwrap();
        let mut index: std::collections::HashMap<String, (Entity, bool)> =
            std::collections::HashMap::new();
        index.insert("/World".to_owned(), (e(0), false));
        index.insert("/World/A".to_owned(), (e(1), false));
        index.insert("/World/A/Mesh".to_owned(), (e(2), true));
        index.insert("/World/B".to_owned(), (e(3), true));
        index.insert("/World/B/Nested".to_owned(), (e(4), true));

        let leaves = leaf_gprims_from_index("/World", &index);
        // The two first-on-branch gprims are collected; the nested one is pruned
        // and the Xforms (and the root) contribute nothing. Order is unspecified.
        assert_eq!(leaves.len(), 2, "expected exactly two leaf gprims, got {leaves:?}");
        assert!(leaves.contains(&e(2)), "/World/A/Mesh must be collected");
        assert!(leaves.contains(&e(3)), "/World/B must be collected");
        assert!(!leaves.contains(&e(4)), "/World/B/Nested is under a gprim → pruned");
    }

    #[test]
    fn partition_highlights_routes_by_style() {
        let entries = vec![
            HighlightEntry {
                path: "/box".into(),
                color: [1.0, 0.0, 0.0, 1.0],
                style: HighlightStyle::Box,
                thickness: 0.0,
            },
            HighlightEntry {
                path: "/tint".into(),
                color: [0.0, 1.0, 0.0, 0.5],
                style: HighlightStyle::Tint,
                thickness: 0.0,
            },
            HighlightEntry {
                path: "/outline".into(),
                color: [0.0, 0.0, 1.0, 1.0],
                style: HighlightStyle::Outline,
                thickness: 6.0,
            },
        ];
        let (boxes, tints, outlines) = partition_highlights(&entries);
        assert_eq!(boxes, vec![("/box".to_string(), [1.0, 0.0, 0.0, 1.0])]);
        assert_eq!(tints.get("/tint"), Some(&[0.0, 1.0, 0.0, 0.5]));
        assert_eq!(tints.len(), 1);
        assert_eq!(
            outlines,
            vec![("/outline".to_string(), [0.0, 0.0, 1.0, 1.0], 6.0)]
        );
    }

    #[test]
    fn resolve_overlay_lines_draws_segments_and_arcs() {
        use glam::{Vec3, Vec4};
        use idtx_behavior::{OverlayCommand, OverlayOp};
        let null = idtx_behavior::abi::Entity::NULL;
        let commands = vec![
            // A near-zero width keeps the crisp 1-px line path: endpoints verbatim.
            OverlayCommand {
                target: null,
                op: OverlayOp::Line {
                    start: Vec3::new(0.0, 0.0, 0.0),
                    end: Vec3::new(1.0, 2.0, 3.0),
                    color: Vec4::new(0.2, 0.9, 0.35, 1.0),
                    width: 0.0,
                },
            },
            // A ballistic arc launched flat and fast; sampled to a multi-segment
            // polyline (more than one line) against the y=0 ground plane.
            OverlayCommand {
                target: null,
                op: OverlayOp::Arc {
                    start: Vec3::new(0.0, 1.5, 0.0),
                    dir: Vec3::new(0.0, 0.2, -1.0),
                    speed: 6.0,
                    gravity: 9.81,
                    color: Vec4::new(0.9, 0.3, 0.2, 1.0),
                    width: 0.0,
                },
            },
            // A prim-scoped highlight must be ignored by the line resolver.
            OverlayCommand {
                target: null,
                op: OverlayOp::Highlight { color: Vec4::ONE },
            },
        ];
        let lines = resolve_overlay_lines(&commands);
        // The single segment plus the multi-segment arc polyline.
        assert!(lines.len() >= 2, "got {} lines", lines.len());
        // First line is the explicit (thin) segment, endpoints preserved verbatim.
        assert_eq!(lines[0].a, [0.0, 0.0, 0.0]);
        assert_eq!(lines[0].b, [1.0, 2.0, 3.0]);
        assert_eq!(lines[0].color, [0.2, 0.9, 0.35, 1.0]);
        // The arc contributed its own coloured segments.
        assert!(lines[1..].iter().any(|l| l.color == [0.9, 0.3, 0.2, 1.0]));
    }

    /// A meaningful authored width is honored: the command is lowered onto a thick
    /// ribbon stroke (offset rails + rungs) rather than a single 1-px line, so the
    /// endpoints are displaced by half the width instead of passed through verbatim.
    #[test]
    fn resolve_overlay_lines_honors_width_as_ribbon() {
        use glam::{Vec3, Vec4};
        use idtx_behavior::{OverlayCommand, OverlayOp};
        let null = idtx_behavior::abi::Entity::NULL;
        // A segment along +X so its ribbon rails offset along ±Z (width × world up).
        let thick = vec![OverlayCommand {
            target: null,
            op: OverlayOp::Line {
                start: Vec3::new(0.0, 0.0, 0.0),
                end: Vec3::new(1.0, 0.0, 0.0),
                color: Vec4::new(1.0, 1.0, 1.0, 1.0),
                width: 0.04,
            },
        }];
        let ribbon = resolve_overlay_lines(&thick);
        // A ribbon emits more than the single line a thin segment would.
        assert!(ribbon.len() > 1, "expected a ribbon, got {} lines", ribbon.len());
        // No emitted vertex sits on the centre line z = 0 at the full half-width:
        // the rails are offset by ±0.02 along Z.
        assert!(
            ribbon.iter().any(|l| (l.a[2].abs() - 0.02).abs() < 1e-5),
            "expected rail vertices offset by half the width",
        );
    }

    /// Spawn a bare prim (PrimNode only) so it can be a `PlayerRole`-carrying
    /// ancestor. Returns the entity + its `PrimId` (to parent children to it).
    fn spawn_prim_node(
        world: &mut World,
        slot: u32,
        path: &str,
        parent: Option<PrimId>,
    ) -> (Entity, PrimId) {
        let prim = PrimId::from_raw_parts(slot, StageGeneration::from_raw(1));
        let e = world
            .spawn(PrimNode {
                prim,
                path: path.to_owned(),
                parent,
                name: path.rsplit('/').next().unwrap_or_default().to_owned(),
                type_name: "Xform".to_owned(),
                state: PrimState {
                    active: true,
                    defined: true,
                    loaded: true,
                },
                is_pseudo_root: false,
                local_layer_identifier: None,
            })
            .id();
        (e, prim)
    }

    /// Spawn a camera prim (PrimNode + CameraComp) under `parent`.
    fn spawn_camera_prim(
        world: &mut World,
        slot: u32,
        path: &str,
        parent: Option<PrimId>,
    ) -> Entity {
        let (e, _) = spawn_prim_node(world, slot, path, parent);
        world.entity_mut(e).insert(CameraComp {
            record: idtx_usd::CameraRecord {
                prim: PrimId::from_raw_parts(slot, StageGeneration::from_raw(1)),
                projection: "perspective".to_owned(),
                focal_length: 50.0,
                horizontal_aperture: 36.0,
                vertical_aperture: 24.0,
                horizontal_aperture_offset: 0.0,
                vertical_aperture_offset: 0.0,
                clipping: [0.1, 1000.0],
                clipping_planes: Vec::new(),
                f_stop: 0.0,
                focus_distance: 0.0,
                stereo_role: String::new(),
                shutter: [0.0, 0.0],
                exposure: 0.0,
                exposure_iso: 0.0,
                exposure_time: 0.0,
                exposure_f_stop: 0.0,
                exposure_responsivity: 0.0,
            },
        });
        e
    }

    #[test]
    fn local_player_camera_none_without_player_role() {
        // A plain scene camera (no PlayerRole ancestor) must NOT be treated as
        // the player camera — the runtime keeps its orbit fallback.
        let mut world = World::new();
        spawn_camera_prim(&mut world, 1, "/World/SceneCam", None);
        assert_eq!(local_player_camera(&mut world), None);
    }

    #[test]
    fn local_player_camera_prefers_pawn_camera_under_local_role() {
        let mut world = World::new();
        // A non-player scene camera first in query order …
        spawn_camera_prim(&mut world, 1, "/World/SceneCam", None);
        // … and the local player's pawn camera under an authored `local` role.
        let (player, player_prim) = spawn_prim_node(&mut world, 2, "/World/Player", None);
        world
            .entity_mut(player)
            .insert(PlayerRole::new(NetRole::Local));
        spawn_camera_prim(
            &mut world,
            3,
            xr_controller_prefab::PLAYER_CAMERA_PATH,
            Some(player_prim),
        );

        let (_, path) = local_player_camera(&mut world).expect("local player camera");
        assert_eq!(path, xr_controller_prefab::PLAYER_CAMERA_PATH);
    }

    #[test]
    fn local_player_camera_ignores_remote_player() {
        let mut world = World::new();
        let (player, player_prim) = spawn_prim_node(&mut world, 1, "/World/Peer", None);
        world
            .entity_mut(player)
            .insert(PlayerRole::new(NetRole::Remote));
        spawn_camera_prim(&mut world, 2, "/World/Peer/Camera", Some(player_prim));
        // A remote player's camera is not a local viewport.
        assert_eq!(local_player_camera(&mut world), None);
    }

    /// The XR eye cameras compose the local pawn root's world matrix. The resolver
    /// returns it only for a `/World/Player` under an authored `Local` role, so a
    /// non-player stage (or a remote pawn) leaves `stage_to_world` identity and the
    /// view at its raw located pose.
    #[cfg(feature = "xr-preview")]
    #[test]
    fn local_player_root_world_resolves_local_pawn_only() {
        use glam::{Mat4, Vec3};

        // No player rig → None (eye render falls back to identity).
        let mut empty = World::new();
        spawn_prim_node(&mut empty, 1, "/World/SceneThing", None);
        assert!(local_player_root_world(&mut empty).is_none());

        // Authored local pawn root carrying a world transform → that matrix.
        let mut world = World::new();
        let (player, _) = spawn_prim_node(&mut world, 1, "/World/Player", None);
        world
            .entity_mut(player)
            .insert(PlayerRole::new(NetRole::Local));
        let m = Mat4::from_translation(Vec3::new(2.0, 0.0, -3.0));
        world.entity_mut(player).insert(WorldTransform { matrix: m });
        let got = local_player_root_world(&mut world).expect("local pawn root");
        assert!(got.abs_diff_eq(m, 1e-6));

        // A remote pawn root is not the local viewport.
        let mut remote = World::new();
        let (peer, _) = spawn_prim_node(&mut remote, 1, "/World/Player", None);
        remote
            .entity_mut(peer)
            .insert(PlayerRole::new(NetRole::Remote));
        remote
            .entity_mut(peer)
            .insert(WorldTransform { matrix: Mat4::IDENTITY });
        assert!(local_player_root_world(&mut remote).is_none());
    }

    #[test]
    fn session_source_local_is_not_remote() {
        let src = SessionSource::Local(PathBuf::from("/tmp/scene.usda"));
        assert!(!src.is_remote());
    }

    #[test]
    fn prim_name_from_asset_takes_sanitised_stem() {
        // Directory components and a single trailing extension are dropped.
        assert_eq!(prim_name_from_asset("assets/props/Table.usdz"), "Table");
        assert_eq!(prim_name_from_asset("Chair.usda"), "Chair");
        // Windows-style separators are tolerated.
        assert_eq!(prim_name_from_asset(r"assets\props\Lamp.usdc"), "Lamp");
        // Illegal identifier characters (spaces, dashes, dots) become '_'.
        assert_eq!(prim_name_from_asset("my cool-prop.usd"), "my_cool_prop");
        assert_eq!(prim_name_from_asset("a.b.usdz"), "a_b");
        // A leading digit is not a legal identifier start → prefixed with '_'.
        assert_eq!(prim_name_from_asset("3d_scan.usdz"), "_3d_scan");
        // Extensionless / empty stems fall back to a stable default.
        assert_eq!(prim_name_from_asset("Robot"), "Robot");
        assert_eq!(prim_name_from_asset(".usdz"), "Asset");
        assert_eq!(prim_name_from_asset(""), "Asset");
    }

    #[test]
    fn shadow_settings_resolution_matrix() {
        use idtx_render::ShadowTier;
        use idtx_usd::ShadowMode;

        // Product forbids shadows (Inspector/Poser): always Off, whatever the
        // stage authored or the hardware allows.
        for mode in [
            ShadowMode::Off,
            ShadowMode::Hard,
            ShadowMode::Smooth,
            ShadowMode::Soft,
        ] {
            for cap in [
                ShadowTier::Off,
                ShadowTier::Hard,
                ShadowTier::Smooth,
                ShadowTier::Soft,
            ] {
                assert_eq!(
                    resolve_shadow_settings(mode, false, cap).tier,
                    ShadowTier::Off,
                    "no capability must force Off (mode={mode:?}, cap={cap:?})"
                );
            }
        }

        // Capability on: authored mode picks the tier 1:1, clamped down by the cap.
        let full = ShadowTier::Soft;
        assert_eq!(
            resolve_shadow_settings(ShadowMode::Off, true, full).tier,
            ShadowTier::Off
        );
        assert_eq!(
            resolve_shadow_settings(ShadowMode::Hard, true, full).tier,
            ShadowTier::Hard
        );
        assert_eq!(
            resolve_shadow_settings(ShadowMode::Smooth, true, full).tier,
            ShadowTier::Smooth
        );
        assert_eq!(
            resolve_shadow_settings(ShadowMode::Soft, true, full).tier,
            ShadowTier::Soft
        );

        // Quest-style Smooth cap: Soft clamps down to Smooth; Hard stays Hard
        // (already below the cap).
        let smooth_cap = ShadowTier::Smooth;
        assert_eq!(
            resolve_shadow_settings(ShadowMode::Soft, true, smooth_cap).tier,
            ShadowTier::Smooth
        );
        assert_eq!(
            resolve_shadow_settings(ShadowMode::Hard, true, smooth_cap).tier,
            ShadowTier::Hard
        );

        // Hard cap: both Smooth and Soft clamp down to the true-hard tier.
        let hard_cap = ShadowTier::Hard;
        assert_eq!(
            resolve_shadow_settings(ShadowMode::Soft, true, hard_cap).tier,
            ShadowTier::Hard
        );
        assert_eq!(
            resolve_shadow_settings(ShadowMode::Smooth, true, hard_cap).tier,
            ShadowTier::Hard
        );

        // Off cap disables everything even with an authored Soft.
        let off_cap = ShadowTier::Off;
        assert_eq!(
            resolve_shadow_settings(ShadowMode::Soft, true, off_cap).tier,
            ShadowTier::Off
        );
    }

    #[test]
    fn shadow_tier_is_ordered_by_cost() {
        use idtx_render::ShadowTier;
        assert!(ShadowTier::Off < ShadowTier::Hard);
        assert!(ShadowTier::Hard < ShadowTier::Smooth);
        assert!(ShadowTier::Smooth < ShadowTier::Soft);
        assert_eq!(ShadowTier::Soft.min(ShadowTier::Smooth), ShadowTier::Smooth);
        assert_eq!(ShadowTier::Soft.min(ShadowTier::Hard), ShadowTier::Hard);
    }

    #[test]
    fn session_source_remote_is_remote() {
        let src = SessionSource::Remote {
            url: "https://example.com/api/v1/files/scene.usda".to_owned(),
        };
        assert!(src.is_remote());
    }

    // ── behavior playback commands → AnimClipPlayer playheads ──────

    /// Spawn an entity carrying an `AnimClipPlayer` with a single paused,
    /// non-looping playhead `clip` over `[start, end]`, and return the wire
    /// entity id a behavior would post commands against.
    fn spawn_player_entity(
        world: &mut idtx_scene::bevy_ecs::world::World,
        clip: &str,
        start: f64,
        end: f64,
    ) -> u64 {
        let mut head = ClipPlayhead::new(clip, start, end, 24.0);
        head.looping = false;
        head.playing = false;
        let mut player = AnimClipPlayer::new();
        player.set_playhead(head);
        world.spawn(player).id().to_bits()
    }

    fn cmd(target: u64, clip: &str, kind: PlaybackCommandKind, value: f32) -> PlaybackCommand {
        PlaybackCommand {
            target: idtx_behavior::abi::Entity::from_bits(target),
            clip: clip.to_owned(),
            kind,
            value,
        }
    }

    #[test]
    fn playback_command_play_seek_speed_loop_mutate_matching_playhead() {
        let mut world = idtx_scene::bevy_ecs::world::World::new();
        let bits = spawn_player_entity(&mut world, "walk", 0.0, 48.0);
        let entity = idtx_scene::bevy_ecs::entity::Entity::try_from_bits(bits).unwrap();

        apply_playback_commands(
            &mut world,
            &[
                cmd(bits, "walk", PlaybackCommandKind::Play, 0.0),
                cmd(bits, "walk", PlaybackCommandKind::Seek, 12.0),
                cmd(bits, "walk", PlaybackCommandKind::SetSpeed, 2.0),
                cmd(bits, "walk", PlaybackCommandKind::SetLooping, 1.0),
            ],
        );

        let player = world.get::<AnimClipPlayer>(entity).unwrap();
        let head = player.playhead("walk").unwrap();
        assert!(head.playing, "Play must start the head");
        assert!(
            (head.current_time_code - 12.0).abs() < 1e-9,
            "Seek moves the head"
        );
        assert!(
            (head.speed - 2.0).abs() < 1e-9,
            "SetSpeed sets the multiplier"
        );
        assert!(head.looping, "SetLooping(1.0) enables looping");

        // Pause then Stop.
        apply_playback_commands(
            &mut world,
            &[cmd(bits, "walk", PlaybackCommandKind::Pause, 0.0)],
        );
        assert!(
            !world
                .get::<AnimClipPlayer>(entity)
                .unwrap()
                .playhead("walk")
                .unwrap()
                .playing
        );
        apply_playback_commands(
            &mut world,
            &[cmd(bits, "walk", PlaybackCommandKind::Stop, 0.0)],
        );
        let head = world
            .get::<AnimClipPlayer>(entity)
            .unwrap()
            .playhead("walk")
            .unwrap()
            .clone();
        assert!(!head.playing, "Stop pauses");
        assert!(
            (head.current_time_code - head.start_time_code).abs() < 1e-9,
            "Stop rewinds to start"
        );
    }

    #[test]
    fn playback_command_play_rewinds_finished_nonlooping_head() {
        let mut world = idtx_scene::bevy_ecs::world::World::new();
        let bits = spawn_player_entity(&mut world, "clap", 0.0, 24.0);
        let entity = idtx_scene::bevy_ecs::entity::Entity::try_from_bits(bits).unwrap();
        // Park the head at the (non-looping) end.
        world
            .get_mut::<AnimClipPlayer>(entity)
            .unwrap()
            .playhead_mut("clap")
            .unwrap()
            .current_time_code = 24.0;

        apply_playback_commands(
            &mut world,
            &[cmd(bits, "clap", PlaybackCommandKind::Play, 0.0)],
        );

        let head = world
            .get::<AnimClipPlayer>(entity)
            .unwrap()
            .playhead("clap")
            .unwrap();
        assert!(head.playing);
        assert!(
            (head.current_time_code - 0.0).abs() < 1e-9,
            "Play on a finished non-looping head restarts it from the beginning",
        );
    }

    #[test]
    fn playback_command_unknown_target_or_clip_is_noop() {
        let mut world = idtx_scene::bevy_ecs::world::World::new();
        let bits = spawn_player_entity(&mut world, "walk", 0.0, 48.0);
        let entity = idtx_scene::bevy_ecs::entity::Entity::try_from_bits(bits).unwrap();

        // Wrong clip label on the right entity, and a Play on a stale entity id.
        apply_playback_commands(
            &mut world,
            &[
                cmd(bits, "run", PlaybackCommandKind::Play, 0.0),
                cmd(
                    bits.wrapping_add(1_000_000),
                    "walk",
                    PlaybackCommandKind::Play,
                    0.0,
                ),
            ],
        );

        let head = world
            .get::<AnimClipPlayer>(entity)
            .unwrap()
            .playhead("walk")
            .unwrap();
        assert!(!head.playing, "no matching playhead was mutated");
    }

    #[test]
    fn transform_only_change_admits_xform_op_writes_but_not_other_domains() {
        // Root-cause regression guard (XR-preview full re-extract): an `xformOp`
        // value write classifies as `TRANSFORM | INSPECTION` in the bridge, so the
        // old exact `== TRANSFORM` gate never fired and every controller pose write
        // forced a full re-extract. The masked test must admit that pair.
        let transform = DirtyDomains::TRANSFORM;
        let inspection = DirtyDomains::INSPECTION;

        // Pure transform and the real xformOp classification both qualify.
        assert!(StudioSession::is_transform_only_change(transform));
        assert!(StudioSession::is_transform_only_change(
            transform.union(inspection)
        ));

        // Missing the TRANSFORM bit never qualifies, even for INSPECTION alone.
        assert!(!StudioSession::is_transform_only_change(inspection));

        // Any bit outside {TRANSFORM, INSPECTION} still needs a full re-extract.
        for extra in [
            DirtyDomains::GEOMETRY,
            DirtyDomains::MATERIAL,
            DirtyDomains::SKELETON,
            DirtyDomains::METADATA,
            DirtyDomains::GENERIC,
            DirtyDomains::TOPOLOGY,
            DirtyDomains::VISIBILITY,
        ] {
            assert!(
                !StudioSession::is_transform_only_change(transform.union(extra)),
                "TRANSFORM combined with {extra:?} must not take the transform-only fast path"
            );
        }
    }

    #[test]
    fn merge_transform_patches_preserves_earlier_passes() {
        // Regression (one jittering body breaks unrelated behaviors): several
        // reprojection passes run per frame and queue deferred patches; the
        // physics write-back runs last and only while a body is awake. Merging
        // must preserve an earlier pass's patch (the camera-followed capsule / a
        // grabbed prim) and let the newer value win for the same path — a plain
        // overwrite silently dropped the behavior patches whenever physics
        // reprojected, so the capsule / grab froze while any body was awake.
        let m = |x: f32| {
            let mut mat = [0.0f32; 16];
            mat[0] = 1.0;
            mat[5] = 1.0;
            mat[10] = 1.0;
            mat[15] = 1.0;
            mat[12] = x; // translation.x
            mat
        };
        let mut pending = None;
        // Pass 1 (a behavior): author the capsule + grabbed prim.
        merge_transform_patches_into(
            &mut pending,
            vec![("/Capsule".to_owned(), m(1.0)), ("/Grabbed".to_owned(), m(2.0))],
        );
        // Pass 2 (physics write-back): a different body moves, and the grabbed
        // prim is re-authored with a newer pose.
        merge_transform_patches_into(
            &mut pending,
            vec![("/Cookie".to_owned(), m(3.0)), ("/Grabbed".to_owned(), m(9.0))],
        );
        let patches = pending.expect("merged patches");
        let get = |path: &str| patches.iter().find(|(p, _)| p == path).map(|(_, mx)| mx[12]);
        assert_eq!(get("/Capsule"), Some(1.0), "earlier pass's capsule must survive");
        assert_eq!(get("/Cookie"), Some(3.0), "later pass's body must be present");
        assert_eq!(get("/Grabbed"), Some(9.0), "same path: newer pose wins");
        assert_eq!(patches.len(), 3, "no duplicate path entries");
    }

    #[test]
    fn physics_authority_change_is_render_irrelevant_only_for_value_writes() {
        // Regression (grab/rest degradation after multiple grab/release): the
        // Phase 2A `physics:startsAsleep` sleep-sync is a value-only `physics:*`
        // write classified `GENERIC`. It must be recognised as render-irrelevant so
        // the reprojection router excludes it and a sleep/wake edge keeps the cheap
        // transform patch instead of a full `UsdCookie`-remarshaling re-extract.
        let prim = PrimId::from_raw_parts(7, StageGeneration::from_raw(1));
        let mk = |name: Option<&str>, resync: bool| idtx_usd::ChangedPrim {
            prim,
            notice_path: "/World/Body".into(),
            property_name: name.map(str::to_owned),
            domains: DirtyDomains::GENERIC.union(DirtyDomains::INSPECTION),
            resync,
            authored_in_root_layer: false,
        };

        // A value-only `physics:*` write is render-irrelevant.
        assert!(StudioSession::is_physics_authority_change(&mk(
            Some("physics:startsAsleep"),
            false
        )));
        // A `physics:*` RESYNC (first-time spec / topology) must still re-hydrate.
        assert!(!StudioSession::is_physics_authority_change(&mk(
            Some("physics:startsAsleep"),
            true
        )));
        // Non-physics writes are never treated as physics-authority.
        assert!(!StudioSession::is_physics_authority_change(&mk(
            Some("xformOp:transform"),
            false
        )));
        assert!(!StudioSession::is_physics_authority_change(&mk(None, false)));
    }

    #[cfg(feature = "authoring")]
    #[test]
    fn ancestor_chain_paths_walks_leaf_to_top_level_prim() {
        assert_eq!(
            ancestor_chain_paths("/Scene/Payload/Detail/Block"),
            vec![
                "/Scene/Payload/Detail/Block".to_owned(),
                "/Scene/Payload/Detail".to_owned(),
                "/Scene/Payload".to_owned(),
                "/Scene".to_owned(),
            ],
            "must reproject every parent up to (but excluding) the pseudo-root",
        );
    }

    #[cfg(feature = "authoring")]
    #[test]
    fn ancestor_chain_paths_top_level_prim_is_its_own_chain() {
        assert_eq!(ancestor_chain_paths("/Scene"), vec!["/Scene".to_owned()]);
    }

    #[cfg(feature = "remote")]
    #[test]
    fn classify_sync_status_maps_connection_facts() {
        // Owner + live + synced → hosting a single/multi-edit session (GREEN).
        assert_eq!(
            classify_sync_status(true, true, true),
            CollabSyncStatus::Hosting
        );
        // Joiner + live + synced → connected to another peer's session (BLUE).
        assert_eq!(
            classify_sync_status(true, true, false),
            CollabSyncStatus::ConnectedToPeer
        );
        // Live but snapshot not yet received → syncing (AMBER), regardless of
        // ownership.
        assert_eq!(
            classify_sync_status(true, false, true),
            CollabSyncStatus::Syncing
        );
        assert_eq!(
            classify_sync_status(true, false, false),
            CollabSyncStatus::Syncing
        );
        // Disconnected takes precedence over everything (RED).
        assert_eq!(
            classify_sync_status(false, true, true),
            CollabSyncStatus::Disconnected
        );
        assert_eq!(
            classify_sync_status(false, false, false),
            CollabSyncStatus::Disconnected
        );
    }

    #[test]
    fn orbit_camera_starts_in_free_perspective_view() {
        let cam = OrbitCamera::default_framing();
        assert_eq!(cam.current_view, None);
        assert_eq!(cam.projection_mode, ProjectionMode::Perspective);
    }

    #[test]
    fn set_view_preset_records_named_view_and_forces_orthographic() {
        let mut cam = OrbitCamera::default_framing();
        cam.set_view_preset(ViewPreset::Top);
        assert_eq!(cam.current_view, Some(ViewPreset::Top));
        assert_eq!(cam.projection_mode, ProjectionMode::Orthographic);
    }

    #[test]
    fn free_orbit_clears_named_view() {
        let mut cam = OrbitCamera::default_framing();
        cam.set_view_preset(ViewPreset::Front);
        assert_eq!(cam.current_view, Some(ViewPreset::Front));
        cam.orbit(0.1, 0.1);
        assert_eq!(cam.current_view, None);
    }

    #[test]
    fn framing_all_clears_named_view() {
        let mut cam = OrbitCamera::default_framing();
        cam.set_view_preset(ViewPreset::Left);
        cam.frame_aabb(Vec3::splat(-1.0), Vec3::splat(1.0));
        assert_eq!(cam.current_view, None);
    }

    /// Screen-right (the view basis' X axis) of the current camera orientation.
    fn screen_right(cam: &OrbitCamera) -> Vec3 {
        let eye = cam.eye();
        let forward = (cam.target - eye).normalize();
        forward.cross(cam.up_hint).normalize()
    }

    #[test]
    fn top_view_has_world_x_as_screen_right() {
        let mut cam = OrbitCamera::default_framing();
        cam.set_view_preset(ViewPreset::Top);
        // Top-down view (image 1): world +X points to screen-right.
        assert!((screen_right(&cam) - Vec3::X).length() < 1e-4);
    }

    #[test]
    fn orbiting_down_from_top_keeps_x_as_screen_right() {
        // Regression: pressing Numpad 2 from the top view must tilt about the
        // screen-X axis, not snap 90° around the vertical axis first. Screen-right
        // must stay world +X across the whole orbit-out transition.
        let mut cam = OrbitCamera::default_framing();
        cam.set_view_preset(ViewPreset::Top);
        cam.orbit(0.0, -15.0_f32.to_radians());
        assert_eq!(cam.current_view, None);
        assert!(
            (screen_right(&cam) - Vec3::X).length() < 1e-4,
            "screen-right drifted to {:?}",
            screen_right(&cam)
        );
        // The camera should now sit above and in front (+Z), not off to +X.
        let dir = (cam.eye() - cam.target).normalize();
        assert!(dir.y > 0.0 && dir.z > 0.0 && dir.x.abs() < 1e-4);
    }

    #[test]
    fn orbiting_up_from_bottom_keeps_minus_x_as_screen_right() {
        let mut cam = OrbitCamera::default_framing();
        cam.set_view_preset(ViewPreset::Bottom);
        let before = screen_right(&cam);
        cam.orbit(0.0, 15.0_f32.to_radians());
        assert!((screen_right(&cam) - before).length() < 1e-4);
    }

    #[test]
    fn pan_in_top_view_follows_screen_axes() {
        // Regression: panning in the top/bottom views was flipped/scrambled
        // because pan() built its basis from `Vec3::Y`, which degenerates when
        // the look direction is parallel to Y (its float residual pointing along
        // the wrong axes). It must use `up_hint`, like the view matrix.
        let mut cam = OrbitCamera::default_framing();
        cam.set_view_preset(ViewPreset::Top);

        // Pan right → target follows screen-right = world +X.
        let start = cam.target;
        cam.pan(1.0, 0.0);
        let d_right = cam.target - start;
        assert!(
            (d_right.normalize() - Vec3::X).length() < 1e-4,
            "pan-right moved {d_right:?}, expected +X"
        );

        // Pan up → target follows screen-up = world -Z (DCC top-down convention).
        let mid = cam.target;
        cam.pan(0.0, 1.0);
        let d_up = cam.target - mid;
        assert!(
            (d_up.normalize() - (-Vec3::Z)).length() < 1e-4,
            "pan-up moved {d_up:?}, expected -Z"
        );
    }

    #[test]
    fn pan_in_bottom_view_follows_screen_axes() {
        let mut cam = OrbitCamera::default_framing();
        cam.set_view_preset(ViewPreset::Bottom);

        // Bottom view: screen-right = world -X, screen-up = world -Z.
        let start = cam.target;
        cam.pan(1.0, 0.0);
        let d_right = cam.target - start;
        assert!(
            (d_right.normalize() - (-Vec3::X)).length() < 1e-4,
            "pan-right moved {d_right:?}, expected -X"
        );

        let mid = cam.target;
        cam.pan(0.0, 1.0);
        let d_up = cam.target - mid;
        assert!(
            (d_up.normalize() - (-Vec3::Z)).length() < 1e-4,
            "pan-up moved {d_up:?}, expected -Z"
        );
    }

    #[test]
    fn pan_in_front_view_is_unchanged() {
        // Non-pole views use up_hint = Y, so the basis (and thus pan) is
        // identical to the previous Vec3::Y implementation: screen-right = +X,
        // screen-up = +Y.
        let mut cam = OrbitCamera::default_framing();
        cam.set_view_preset(ViewPreset::Front);

        let start = cam.target;
        cam.pan(1.0, 0.0);
        let d_right = cam.target - start;
        assert!(
            (d_right.normalize() - Vec3::X).length() < 1e-4,
            "pan-right moved {d_right:?}, expected +X"
        );

        let mid = cam.target;
        cam.pan(0.0, 1.0);
        let d_up = cam.target - mid;
        assert!(
            (d_up.normalize() - Vec3::Y).length() < 1e-4,
            "pan-up moved {d_up:?}, expected +Y"
        );
    }

    #[test]
    fn synthetic_framing_is_45_deg_top_right() {
        // The no-camera fallback must frame from 45° azimuth / 45° elevation.
        for cam in [
            OrbitCamera::default_framing(),
            {
                let mut c = OrbitCamera::default_framing();
                c.frame_aabb(Vec3::splat(-2.0), Vec3::splat(2.0));
                c
            },
        ] {
            assert!((cam.yaw - std::f32::consts::FRAC_PI_4).abs() < 1e-5);
            assert!((cam.pitch - std::f32::consts::FRAC_PI_4).abs() < 1e-5);
        }
    }

    #[test]
    fn set_from_eye_target_round_trips_through_eye() {
        // Seeding orbit params from an eye/target (the scene-camera hand-off)
        // must reproduce that eye position via `eye()`.
        let mut cam = OrbitCamera::default_framing();
        let eye = Vec3::new(6.0, 5.0, 9.0);
        let target = Vec3::new(0.0, 1.0, 0.0);
        cam.set_from_eye_target(eye, target);
        assert_eq!(cam.current_view, None);
        assert_eq!(cam.projection_mode, ProjectionMode::Perspective);
        assert!(
            (cam.eye() - eye).length() < 1e-3,
            "reconstructed eye {:?} != {:?}",
            cam.eye(),
            eye
        );
        assert!((cam.target - target).length() < 1e-5);
    }

    // ── Collaboration transform tests ───────────────────────────────────────

    /// Helper: build a USD flat row-major matrix from TRS.
    /// Only tests translation and identity rotation/scale so we don't need the
    /// full XYZ Euler formula in the test itself.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    fn usd_identity_with_translation(tx: f64, ty: f64, tz: f64) -> idtx_collab::conv::UsdMat4 {
        [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, tx, ty, tz, 1.0,
        ]
    }

    #[cfg(all(feature = "remote", feature = "authoring"))]
    fn trs_op(op_type: idtx_usd::XformOpType, name: &str, value: [f64; 3]) -> idtx_usd::AuthoredXformOp {
        let op_type_name = match op_type {
            idtx_usd::XformOpType::Translate => "translate",
            idtx_usd::XformOpType::RotateXyz => "rotateXYZ",
            idtx_usd::XformOpType::Scale => "scale",
            idtx_usd::XformOpType::Unsupported => "transform",
        };
        idtx_usd::AuthoredXformOp {
            name: name.to_owned(),
            op_type,
            op_type_name: op_type_name.to_owned(),
            precision: idtx_usd::XformPrecision::Double,
            inverse: false,
            time_sample_count: 0,
            has_default: true,
            value,
        }
    }

    /// The core regression: a prim scaled non-uniformly on X and Y and rotated
    /// 90° about X must yield the exact authored TRS — the Y scale must stay on
    /// Y. Baking to a matrix for the server to decompose is what moved it to Z.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    #[test]
    #[allow(clippy::float_cmp)]
    fn trs_ops_for_outbound_keeps_scale_exact_under_quarter_turn() {
        use idtx_usd::XformOpType;
        for rot_x in [90.0_f64, -90.0] {
            let ops = vec![
                trs_op(XformOpType::Translate, "xformOp:translate", [1.0, 2.0, 3.0]),
                trs_op(XformOpType::RotateXyz, "xformOp:rotateXYZ", [rot_x, 0.0, 0.0]),
                trs_op(XformOpType::Scale, "xformOp:scale", [2.0, 3.0, 1.0]),
            ];
            let (t, r, s) =
                trs_ops_for_outbound(&ops, true).expect("pure TRS stack must extract");
            assert_eq!(t, [1.0, 2.0, 3.0]);
            assert_eq!(r, [rot_x, 0.0, 0.0]);
            assert_eq!(
                s, [2.0, 3.0, 1.0],
                "Y scale (3.0) must stay on Y at rot_x={rot_x}, not migrate to Z"
            );
        }
    }

    /// Absent ops take identity defaults (scale 1, rotation 0, translation 0),
    /// matching `classify_authored_transform`.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    #[test]
    #[allow(clippy::float_cmp)]
    fn trs_ops_for_outbound_defaults_absent_ops() {
        use idtx_usd::XformOpType;
        let ops = vec![trs_op(XformOpType::Scale, "xformOp:scale", [5.0, 6.0, 7.0])];
        let (t, r, s) = trs_ops_for_outbound(&ops, true).expect("scale-only stack is pure TRS");
        assert_eq!(t, [0.0, 0.0, 0.0]);
        assert_eq!(r, [0.0, 0.0, 0.0]);
        assert_eq!(s, [5.0, 6.0, 7.0]);
        // An empty stack is the identity TRS.
        assert_eq!(
            trs_ops_for_outbound(&[], true),
            Some(([0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]))
        );
    }

    /// Non-TRS or non-exact stacks fall back to `None` (the matrix path): a
    /// matrix op, an inverse op, an animated-only op, an `orient`/non-`rotateXYZ`
    /// rotate, or a non-editable stack.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    #[test]
    #[allow(clippy::float_cmp)]
    fn trs_ops_for_outbound_rejects_non_exact_stacks() {
        use idtx_usd::XformOpType;

        // A lone matrix op → Unsupported op_type → matrix fallback.
        let matrix = vec![trs_op(XformOpType::Unsupported, "xformOp:transform", [0.0; 3])];
        assert_eq!(trs_ops_for_outbound(&matrix, true), None);

        // `orient` / rotateX etc. also arrive as Unsupported.
        let mut orient = trs_op(XformOpType::Unsupported, "xformOp:orient", [0.0; 3]);
        orient.op_type_name = "orient".to_owned();
        assert_eq!(trs_ops_for_outbound(&[orient], true), None);

        // An inverse op is rejected even if it is a T/R/S type.
        let mut inverse = trs_op(XformOpType::Translate, "xformOp:translate", [1.0, 0.0, 0.0]);
        inverse.inverse = true;
        assert_eq!(trs_ops_for_outbound(&[inverse], true), None);

        // An animated-only op (no authored default) is rejected.
        let mut animated = trs_op(XformOpType::Scale, "xformOp:scale", [2.0, 3.0, 1.0]);
        animated.has_default = false;
        assert_eq!(trs_ops_for_outbound(&[animated], true), None);

        // A pivot op is rejected.
        let pivot = trs_op(XformOpType::Translate, "xformOp:translate:pivot", [0.0; 3]);
        assert_eq!(trs_ops_for_outbound(&[pivot], true), None);

        // A non-editable stack is rejected regardless of op shape.
        let scale = vec![trs_op(XformOpType::Scale, "xformOp:scale", [2.0, 3.0, 1.0])];
        assert_eq!(trs_ops_for_outbound(&scale, false), None);
    }

    #[cfg(all(feature = "remote", feature = "authoring"))]
    #[test]
    fn world_to_local_matrix_identity_parent_is_world() {
        // With an identity parent the local transform equals the world transform.
        let identity = usd_identity_with_translation(0.0, 0.0, 0.0);
        let world = usd_identity_with_translation(3.0, 7.0, -2.0);
        let local = idtx_usd::usd_world_to_local(&world, &identity).unwrap();
        assert!(local.iter().zip(world).all(|(a, b)| (a - b).abs() < 1e-12));
    }

    #[cfg(all(feature = "remote", feature = "authoring"))]
    #[test]
    fn world_to_local_matrix_subtracts_parent_translation() {
        // A parent translated by (10,20,30) and a child at world (12,23,34) must
        // yield a local translation of (2,3,4) — world minus parent.
        let parent = usd_identity_with_translation(10.0, 20.0, 30.0);
        let world = usd_identity_with_translation(12.0, 23.0, 34.0);
        let local = idtx_usd::usd_world_to_local(&world, &parent).unwrap();
        let expected = usd_identity_with_translation(2.0, 3.0, 4.0);
        assert!(local.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-12));
    }

    // ── XR controller input glue (U-XR) ───────────────────────────────────────

    /// The platform→behavior field mapping, and the feed resolving a bound
    /// action end-to-end through the input router (the two glue seams
    /// `tick_behaviors` wires together for a live XR session).
    #[cfg(feature = "xr-preview")]
    #[test]
    fn xr_controller_input_feeds_bound_actions() {
        use idtx_behavior::{InputMap, InputRouter};
        use idtx_platform_xr::ControllerButtons;

        let left = ControllerButtons {
            thumbstick: [0.3, -0.6],
            primary: true, // X on the left controller
            ..ControllerButtons::default()
        };
        let right = ControllerButtons {
            trigger: 0.9,
            ..ControllerButtons::default()
        };

        // Field mapping (platform → behavior) is faithful.
        let snap = xr_controller_snapshot(&left, &right);
        assert_eq!(snap.left.thumbstick, [0.3, -0.6]);
        assert!(snap.left.primary);
        assert!((snap.right.trigger - 0.9).abs() < 1e-6);

        // End-to-end: feed → router resolves move (axis2), jump (X), fire (trigger).
        let map_src = r#"
[actions.move]
kind = "axis2"
bindings = ["Controller/MetaQuest/LeftThumbstick"]
[actions.jump]
kind = "button"
bindings = ["Controller/MetaQuest/X"]
[actions.fire]
kind = "button"
bindings = ["Controller/MetaQuest/RightTrigger"]
"#;
        let mut router = InputRouter::new(InputMap::parse(map_src).unwrap());
        let mut fi = idtx_behavior::FrameInput::new();
        idtx_behavior::apply_xr_controllers(&mut fi, &snap);
        let out = router.snapshot(&fi.raw(), 0.016);

        let mv = out.axis2(router.resolve("move"));
        assert!((mv[0] - 0.3).abs() < 1e-6 && (mv[1] + 0.6).abs() < 1e-6);
        assert_eq!(
            out.phase(router.resolve("jump")),
            idtx_behavior::abi::InputPhase::Pressed
        );
        assert_eq!(
            out.phase(router.resolve("fire")),
            idtx_behavior::abi::InputPhase::Pressed
        );
    }

    /// The head pose fed to XR behaviors is the eye-position midpoint with the
    /// (shared) eye orientation — regression lock for the pcvr HMD feed that
    /// `xr_player_move` / `hmd_camera_sync` gate on (without it both early-return
    /// every frame and the thumbstick never moves the pawn).
    #[cfg(idtx_xr)]
    #[test]
    fn head_pose_is_eye_midpoint_with_shared_orientation() {
        use idtx_openxr_sys::LocatedPose;

        // Eyes 6 cm apart on X, 1.6 m up, both facing the same way (90° yaw).
        let yaw = glam::Quat::from_rotation_y(core::f32::consts::FRAC_PI_2);
        let left = LocatedPose {
            position: [-0.03, 1.6, -0.5],
            orientation: yaw.to_array(),
        };
        let right = LocatedPose {
            position: [0.03, 1.6, -0.5],
            orientation: yaw.to_array(),
        };

        let head = head_pose_from_eyes(left, right);

        // Centre = midpoint (the X components cancel).
        assert!((head.position - glam::Vec3::new(0.0, 1.6, -0.5)).length() < 1e-6);
        // Orientation is the shared eye orientation, carried through intact:
        // the forward vector the gaze-relative behaviors read (`rotation * -Z`)
        // matches the eyes'.
        let head_fwd = head.orientation * glam::Vec3::NEG_Z;
        let eye_fwd = yaw * glam::Vec3::NEG_Z;
        assert!((head_fwd - eye_fwd).length() < 1e-6, "head_fwd = {head_fwd:?}");
    }

    /// `recenter_step`: holds within the threshold, snaps to the head ground
    /// past it, and always returns a ground-plane (y = 0) vector.
    #[test]
    fn recenter_step_holds_then_snaps_on_ground() {
        let threshold = RECENTER_THRESHOLD;

        // Within threshold of the origin → hold ZERO.
        let near = glam::Vec3::new(0.02, 1.6, -0.02); // ground dist ≈ 0.028 < 0.05
        assert_eq!(
            recenter_step(glam::Vec3::ZERO, near, threshold),
            glam::Vec3::ZERO
        );

        // Beyond threshold → snap to the head's ground projection (y dropped).
        let far = glam::Vec3::new(0.4, 1.6, -0.9);
        let snapped = recenter_step(glam::Vec3::ZERO, far, threshold);
        assert_eq!(snapped, glam::Vec3::new(0.4, 0.0, -0.9));

        // Once snapped, a head still near the new origin holds.
        let held = recenter_step(snapped, glam::Vec3::new(0.42, 1.6, -0.92), threshold);
        assert_eq!(held, snapped);

        // flatten_ground drops only Y.
        assert_eq!(
            flatten_ground(glam::Vec3::new(1.0, 9.0, -2.0)),
            glam::Vec3::new(1.0, 0.0, -2.0)
        );
    }
}
