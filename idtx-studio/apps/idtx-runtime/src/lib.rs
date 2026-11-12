//! Standalone cdylib entry points for the packaged player.
//!
//! One crate, one `cdylib` (`libidtx_runtime.{so,dylib}`), two foreign-target
//! entry points:
//!
//! * **Android / Quest** (`aarch64-linux-android`): [`android_main`] is the
//!   `NativeActivity` entry winit's `android-native-activity` glue (via
//!   `android-activity`) calls. It hands the live `AndroidApp` to
//!   [`idtx_runtime_core::run_android`], which extracts the bundled payload from
//!   the APK `assets/`, sets up USD plugin discovery, and runs the player
//!   (XR by default on the `quest` build).
//! * **Apple Vision Pro** (`aarch64-apple-visionos`): [`idtx_runtime_visionos_main`]
//!   is a C-ABI entry the visionOS app shell calls with a USD stage path. It opens
//!   that stage through the winit-free `idtx-usd` wrapper — proving the `OpenUSD`
//!   bridge works on the visionOS runtime — and is driven in the Simulator by
//!   `cargo xtask run-visionos-sim`. It deliberately does **not** pull
//!   `idtx-runtime-core`: `winit` 0.30 has no visionOS support (it classifies the
//!   target as a "free unix" and drags the X11/Wayland backend, which does not
//!   build), so the full windowed/immersive player — `CompositorServices` + Metal +
//!   ARKit — is a later, device-bound phase.
//!
//! On every other target (desktop hosts) this crate is intentionally **empty**:
//! the desktop/XR player lives in the `idtx-runtime` *binary* (`src/main.rs`).
//! The empty cdylib still gets produced on a desktop host but is never shipped.

// ── Android / Quest ──────────────────────────────────────────────────────────

#[cfg(any(target_os = "android", target_os = "visionos"))]
mod build_info;

#[cfg(target_os = "android")]
use android_activity::AndroidApp;

/// `NativeActivity` entry point for the Quest build.
///
/// The `android-activity` native-activity glue calls this `#[no_mangle]` symbol
/// with the live [`AndroidApp`]. We install a logcat-visible tracing subscriber
/// and hand off to [`idtx_runtime_core::run_android`], which performs the
/// on-device payload bootstrap (APK asset extraction + `PXR_PLUGINPATH_NAME` +
/// run-config resolution) and drives the player loop. There is no argv on
/// Android, so stage/mode come only from the bundled `run.toml`.
///
/// # Safety
///
/// `#[unsafe(no_mangle)]` exports the exact symbol the activity glue links
/// against; there is no other caller.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    build_info::retain();
    init_android_tracing();
    tracing::info!("idtx-runtime android_main starting");

    if let Err(e) = idtx_runtime_core::run_android(app) {
        tracing::error!("idtx-runtime: android run failed: {e}");
    }
}

/// Install a `tracing` subscriber whose events reach `adb logcat`.
///
/// `android-activity`'s native-activity glue redirects the process stdout/stderr
/// to logcat, so a plain stderr writer (matching the desktop `main`'s subscriber)
/// surfaces there. `try_init` is used so a re-entry (resume after suspend) does
/// not panic on an already-installed global subscriber.
#[cfg(target_os = "android")]
fn init_android_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

// ── Apple Vision Pro ─────────────────────────────────────────────────────────

/// C-ABI entry point for the visionOS build.
///
/// Invoked by the visionOS app shell (the Simulator probe app assembled by
/// `cargo xtask run-visionos-sim`). It installs a stderr
/// tracing subscriber and **opens the USD stage at `stage_path`** through the
/// winit-free [`idtx_usd`] bridge — proving the `OpenUSD` cxx bridge
/// (`idtx-usd-sys` + the visionOS USD monolith) is linked and works on the
/// visionOS runtime. It does **not** pull `idtx-runtime-core` (whose `winit`
/// dependency has no visionOS support). The immersive session (`CompositorServices`
/// + Metal + ARKit render) replaces this later.
///
/// Returns `0` on success, `1` if the stage failed to open, `2` if `stage_path`
/// is null or not valid UTF-8 — so the caller (and the probe app) can assert.
///
/// # Safety
///
/// `#[unsafe(no_mangle)]` exports the exact `extern "C"` symbol the app shell
/// links against. `stage_path` must be either null or a valid NUL-terminated C
/// string that stays alive for the duration of the call; there is no other caller.
#[cfg(target_os = "visionos")]
#[unsafe(no_mangle)]
pub extern "C" fn idtx_runtime_visionos_main(stage_path: *const std::os::raw::c_char) -> i32 {
    build_info::retain();
    init_visionos_tracing();
    tracing::info!("idtx-runtime idtx_runtime_visionos_main starting");

    if stage_path.is_null() {
        tracing::error!("idtx-runtime: visionos entry called with a null stage path");
        return 2;
    }
    // SAFETY: the caller contract (see fn docs) guarantees a valid NUL-terminated
    // C string living for the call; we copy it out immediately.
    let path = match unsafe { std::ffi::CStr::from_ptr(stage_path) }.to_str() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            tracing::error!("idtx-runtime: visionos stage path is not valid UTF-8: {e}");
            return 2;
        }
    };

    // Open the bundled USD stage via the (winit-free) bridge. Success proves the
    // OpenUSD monolith + cxx shim are linked and callable on the visionOS runtime.
    match idtx_usd::StageService::open(&path) {
        Ok(_stage) => {
            tracing::info!(stage = %path, "idtx-runtime: opened USD stage on visionOS");
            0
        }
        Err(e) => {
            tracing::error!(stage = %path, "idtx-runtime: failed to open USD stage: {e}");
            1
        }
    }
}

/// Install a `tracing` subscriber for the visionOS bring-up entry, writing to
/// `idtx_visionos.log` in the app's writable temp dir — **not** stderr, which a
/// visionOS app black-holes (so the HTTP resolver's fetch logs, among others,
/// were previously invisible on device). Same file as [`visionos_diag`], so the
/// two interleave and pull together.
///
/// `try_init` is used so a re-entry (the shell re-opening the immersive space)
/// does not panic on an already-installed global subscriber. Also installs the
/// crash-diagnostic panic hook (idempotent) so an "immediate crash" leaves a
/// line in `idtx_visionos.log` instead of a silent `panic=abort`.
#[cfg(target_os = "visionos")]
fn init_visionos_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    install_visionos_panic_hook();
    // Default to info, but surface the HTTP resolver's per-fetch debug lines
    // (attempt / status / cached) so remote-asset bring-up is diagnosable from
    // the pulled log. Overridable via `RUST_LOG`/env.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,idtx_usd_resolver_http=debug"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(|| -> Box<dyn std::io::Write> {
            // Append to the pullable log; fall back to stderr if it cannot open.
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(std::env::temp_dir().join("idtx_visionos.log"))
            {
                Ok(f) => Box::new(f),
                Err(_) => Box::new(std::io::stderr()),
            }
        })
        .try_init();
}

/// Append a diagnostic line to `idtx_visionos.log` in the app's **writable temp
/// dir** (`TMPDIR` → `<container>/tmp`). The sandbox root (`$HOME`) is not
/// writable on device, so writing there silently no-ops; the temp dir works on
/// both the Simulator and a physical device and is pullable with
/// `xcrun devicectl device copy from --domain-type appDataContainer
/// --domain-identifier <bundle-id> --source tmp/idtx_visionos.log …`.
///
/// This is the shared sink used by both the stage-open probe entry and the
/// immersive session FFI, **and** by the panic hook — the Rust `tracing` stderr
/// writer is not routed to the unified log in a visionOS app, so this file is how
/// bring-up (and crashes) are inspected. Best-effort.
#[cfg(target_os = "visionos")]
pub(crate) fn visionos_diag(msg: &str) {
    use std::io::Write as _;
    let path = std::env::temp_dir().join("idtx_visionos.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        let _ = writeln!(f, "[rust {ts:.3}] {msg}");
    }
}

/// Install a one-shot panic hook that records the panic payload + location to
/// `idtx_visionos.log` before the process aborts.
///
/// The visionOS cross-build is compiled `panic=abort` (tier-3 `-Z build-std`), so
/// an unwinding-free panic terminates the app with no stderr reaching any
/// pullable log. Capturing it here turns an otherwise opaque "immediate crash"
/// into a diagnosable line (e.g. an `OpenUSD` plugin-registration failure, a Metal
/// bring-up `expect`, or a drawable-import assertion). Chains to the previous hook
/// so default behaviour is preserved. Idempotent via [`std::sync::Once`].
#[cfg(target_os = "visionos")]
fn install_visionos_panic_hook() {
    use std::sync::Once;
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let loc = info
                .location()
                .map_or_else(|| "<unknown>".to_owned(), |l| format!("{}:{}", l.file(), l.line()));
            let msg = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic payload>".to_owned());
            visionos_diag(&format!("PANIC at {loc}: {msg}"));
            prev(info);
        }));
    });
}

// ── Apple Vision Pro immersive renderer ──────────────────────────

/// The `CompositorServices` + Metal + ARKit immersive render path, driven by the
/// Swift app shell. Compiled only for the visionOS cross-build with the
/// `visionos-immersive` feature; the stage-open probe
/// ([`idtx_runtime_visionos_main`]) stays available either way.
///
/// # Ownership / threading contract
///
/// The Swift shell owns the `CompositorServices` render loop (a dedicated thread
/// per `cp_layer_renderer`). It calls, in order:
///
/// 1. [`idtx_visionos_session_create`] once, with the stage path + the
///    `LayerRenderer`'s `MTLDevice`, receiving an opaque session handle.
/// 2. [`idtx_visionos_render_frame`] every frame with the current drawable's
///    per-eye color `MTLTexture`s + ARKit-sourced head/eye poses, which renders
///    the stereo frame into those textures. The shell then `encodePresent`s the
///    drawable on its own command queue (the wgpu work is already queued and
///    ordered on the shared Metal device).
/// 3. [`idtx_visionos_session_destroy`] once, when the immersive space closes.
///
/// All three run on the shell's render thread; the session handle is **not**
/// `Send`/`Sync` and must only be used from that thread.
#[cfg(all(target_os = "visionos", feature = "visionos-immersive"))]
#[allow(clippy::doc_markdown)]
mod visionos_immersive {
    use std::ffi::{CStr, c_char, c_void};

    use idtx_behavior::abi::{
        XR_HAND_JOINT_COUNT, Xform, XrHandJointResult, XrInputSource, XrPoseResult,
        XrSide, XrSpace,
    };
    use idtx_behavior::{FrameInput, XrSnapshot};
    use idtx_platform_desktop::DesktopGpu;
    use idtx_platform_visionos::{CompositorServicesBackend, EyeViewInput};
    use idtx_platform_xr::view::{HmdPose, StereoViews};
    use idtx_platform_xr::{Hand, HandJoint, HandPose, XrBackend};
    #[cfg(feature = "gesture-recognition")]
    use idtx_platform_xr::{
        GestureConfig, GestureLink, GesturePhase, GestureWorker, HandSnapshot, SimpleGestureConfig,
        point_diagnostics, thumb_index_gap,
    };
    use idtx_studio_session::StudioSession;
    use idtx_session_orchestration::{LevelTable, SessionSet, TransitionDriver};
    use idtx_studio_session::visionos_render::VisionosEyeFrame;

    /// Maximum eyes/views per frame (Vision Pro is stereo).
    const MAX_EYES: usize = 2;

    /// One eye's per-frame input from the Swift shell: the `cp_drawable` color
    /// `MTLTexture` to present into, plus that eye's located pose + frustum
    /// tangents (from the drawable view + ARKit device anchor).
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct IdtxEyeInput {
        /// `id<MTLTexture>` of this eye's drawable color texture.
        pub color_texture: *mut c_void,
        /// `id<MTLTexture>` of this eye's drawable depth texture (Depth32Float).
        /// The device compositor requires per-pixel depth for reprojection; we
        /// resolve + copy the eye's depth into it. May be null if absent.
        pub depth_texture: *mut c_void,
        /// Eye position in STAGE space (metres).
        pub pos: [f32; 3],
        /// Eye orientation quaternion `(x, y, z, w)`.
        pub orientation: [f32; 4],
        /// Frustum tangents `(left, right, up, down)` — `tan(angle)`; left/down
        /// are typically negative, matching the off-axis projection.
        pub tangents: [f32; 4],
    }

    /// A full immersive frame from the Swift shell.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct IdtxFrameInput {
        /// Head position in STAGE space (metres).
        pub head_pos: [f32; 3],
        /// Head orientation quaternion `(x, y, z, w)`.
        pub head_orientation: [f32; 4],
        /// Per-eye inputs; only the first `view_count` are read.
        pub eyes: [IdtxEyeInput; MAX_EYES],
        /// Number of valid eyes (2 on Vision Pro).
        pub view_count: u32,
        /// Drawable width in pixels (both eyes share the size).
        pub width: u32,
        /// Drawable height in pixels.
        pub height: u32,
        /// `MTLPixelFormat` raw value of the drawable color texture (mapped to
        /// the matching wgpu format for the blit pipeline).
        pub color_pixel_format: u32,
        /// Seconds since the previous frame (drives animation + behaviors).
        pub dt_seconds: f32,
        /// Predicted presentation time (seconds) for pose timestamps.
        pub timestamp: f64,
    }

    /// One located hand joint pushed from the Swift shell (ARKit
    /// `HandTrackingProvider`). STAGE space; `XR_HAND_JOINT_*_EXT` ordinals.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct IdtxHandJoint {
        /// Joint position in STAGE space (metres).
        pub position: [f32; 3],
        /// Joint capsule radius (metres); `<= 0` falls back to a 5 mm default.
        pub radius: f32,
        /// Joint orientation quaternion `(x, y, z, w)`.
        pub orientation: [f32; 4],
    }

    /// One hand's full 26-joint skeleton for the current frame, pushed via
    /// [`idtx_visionos_session_set_hand`]. Mirrors the `OpenXR` 26-joint hand so
    /// visionOS behaviors read the same `ctx.xr().hand_joints(side)` data as
    /// Quest/PCVR.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct IdtxHandPose {
        /// 26 joints in `XR_HAND_JOINT_*_EXT` order (PALM = 0, WRIST = 1, …).
        pub joints: [IdtxHandJoint; XR_HAND_JOINT_COUNT],
    }

    /// Opaque immersive session handed back to the Swift shell as a raw pointer.
    pub struct VisionosSession {
        /// The live level plus an optional resident transfer/mask session (the
        /// per-level-assembly level transition). A single-level experience holds
        /// a set with no transfer, so `active_session_mut()` is always the one
        /// level and the frame loop is unchanged from the pre-transition path.
        sessions: SessionSet,
        /// Services behavior-posted level transitions (cut to the mask, async-load
        /// the target, place the player, swap). `None` for a single-level
        /// experience (`[experience].levels` empty) — the loop then never polls
        /// one. The winit-free visionOS runtime builds this directly (it cannot
        /// depend on `idtx-runtime-core`); the driver reuses the shared
        /// `idtx-session-orchestration` state machine, exactly as the
        /// desktop/Quest player does. No XR controller `Model` injection layer is
        /// carried (visionOS has no tracked controllers), so swapped-in levels
        /// open with an empty session layer.
        driver: Option<TransitionDriver>,
        backend: CompositorServicesBackend,
        /// Per-frame input accumulator fed into the behavior host before
        /// `run_frame` (parity with the `OpenXR` `tick_behaviors`): hand-gesture
        /// sources are pressed onto it, then `update_input` resolves bound
        /// actions (`right_grab`/`left_grab`, …).
        frame_input: FrameInput,
        /// Background hand-gesture recogniser (pinch/fist/…) driven from the
        /// ARKit hand poses each frame. `None` when recognition is disabled.
        #[cfg(feature = "gesture-recognition")]
        gesture: Option<GestureLink>,
        /// Gesture source strings active last frame (for the router's edge
        /// derivation), e.g. `Gesture/pinch/right`.
        #[cfg(feature = "gesture-recognition")]
        gesture_prev: std::collections::HashSet<String>,
        /// Monotonic frame counter fed to the gesture worker (diagnostics).
        #[cfg(feature = "gesture-recognition")]
        gesture_frame: u64,
        /// Last-logged (left, right) hand-tracking presence, so a None↔Some
        /// transition is logged once rather than every frame.
        #[cfg(feature = "gesture-recognition")]
        diag_hands: (bool, bool),
        /// Last-logged (left, right) `Point` verdict, so the point-pose
        /// diagnostic logs on each register↔drop flip (plus a periodic refresh).
        #[cfg(feature = "gesture-recognition")]
        diag_point: (bool, bool),
        /// Count of frames rendered — used only to emit a one-shot diagnostic
        /// confirming the per-eye render path actually ran on the Simulator.
        frame_count: u64,
        presentation_queue_shared: bool,
        #[cfg(feature = "metrics")]
        frame_metrics: idtx_session_orchestration::frame_metrics::FrameMetrics,
        /// Keeps the wgpu Metal device (shared with `CompositorServices`) alive for
        /// the session's lifetime; the renderer borrows clones of its queue/device.
        _gpu: DesktopGpu,
    }


    /// Borrow the render queue for ordered Swift presentation. The shell must
    /// retain it and submit presentation on this queue after render_frame.
    #[unsafe(no_mangle)]
    pub extern "C" fn idtx_visionos_command_queue(session: *mut VisionosSession) -> *mut c_void {
        if session.is_null() { return std::ptr::null_mut(); }
        // SAFETY: the FFI owner supplies its live, exclusively accessed session.
        let session = unsafe { &mut *session };
        let Some(handles) = session._gpu.metal_handles() else { return std::ptr::null_mut(); };
        session.presentation_queue_shared = true;
        handles.command_queue
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn idtx_visionos_metrics_enabled() -> bool { cfg!(feature = "metrics") }

    #[unsafe(no_mangle)]
    pub extern "C" fn idtx_visionos_frame_complete(session: *mut VisionosSession, elapsed_ns: u64) {
        #[cfg(feature = "metrics")]
        if !session.is_null() {
            // SAFETY: called serially by the same Swift owner after submission.
            unsafe {
                if elapsed_ns == 0 { (*session).frame_metrics.reset_pacing(); }
                else { (*session).frame_metrics.record_external(elapsed_ns); }
            }
        }
        #[cfg(not(feature = "metrics"))]
        let _ = (session, elapsed_ns);
    }

    /// Create an immersive session: adopt the shell's Metal device into wgpu, open
    /// the stage, attach Pulley behaviors, and return an opaque handle.
    ///
    /// Returns null on any failure (bad path, GPU bring-up, or stage open) —
    /// errors are logged. `mtl_device` is the `LayerRenderer.device`
    /// `id<MTLDevice>`; on single-GPU Vision Pro it is the system-default device
    /// wgpu also resolves, so drawable textures import validly.
    ///
    /// # Safety
    ///
    /// `stage_path` must be a valid NUL-terminated C string; `mtl_device` a live
    /// `id<MTLDevice>` (or null). Called once from the shell's render thread.
    #[unsafe(no_mangle)]
    pub extern "C" fn idtx_visionos_session_create(
        stage_path: *const c_char,
        mtl_device: *mut c_void,
    ) -> *mut VisionosSession {
        // Earliest breadcrumb: proves the FFI boundary was crossed (i.e. the
        // Swift shell + dyld dylib closure loaded) before anything else runs, so
        // a crash BEFORE this line is localized to load-time / the Swift shell.
        diag("idtx_visionos_session_create entered");
        super::init_visionos_tracing();

        if stage_path.is_null() {
            tracing::error!("idtx_visionos_session_create: null stage path");
            return std::ptr::null_mut();
        }
        // SAFETY: caller contract — a valid NUL-terminated C string for the call.
        let path = match unsafe { CStr::from_ptr(stage_path) }.to_str() {
            Ok(s) => s.to_owned(),
            Err(e) => {
                tracing::error!("idtx_visionos_session_create: stage path not UTF-8: {e}");
                return std::ptr::null_mut();
            }
        };

        match build_session(&path, mtl_device) {
            Ok(session) => Box::into_raw(Box::new(session)),
            Err(e) => {
                tracing::error!(stage = %path, "idtx_visionos_session_create failed: {e}");
                diag(&format!("session_create FAILED: {e}"));
                std::ptr::null_mut()
            }
        }
    }

    /// Push one hand's located ARKit joints for the current frame. `side`: 0 =
    /// left, 1 = right. Pass `pose == null` to clear the hand (lost tracking).
    /// Call once per tracked hand per frame, BEFORE the next render; the render
    /// feeds the joints into the behavior host snapshot (so
    /// `ctx.xr().hand_joints(side)` works, parity with `OpenXR`) and into the
    /// gesture recogniser (pinch → `*_grab`).
    ///
    /// # Safety
    ///
    /// `session` must be a live handle from [`idtx_visionos_session_create`];
    /// `pose`, when non-null, a valid `IdtxHandPose` for the call's duration.
    #[unsafe(no_mangle)]
    pub extern "C" fn idtx_visionos_session_set_hand(
        session: *mut VisionosSession,
        side: u32,
        pose: *const IdtxHandPose,
    ) {
        // SAFETY: caller contract — a live session handle.
        let Some(s) = (unsafe { session.as_mut() }) else {
            return;
        };
        let hand = if side == 1 { Hand::Right } else { Hand::Left };
        let pose = if pose.is_null() {
            None
        } else {
            // SAFETY: caller contract — a valid `IdtxHandPose` for the call.
            Some(to_hand_pose(unsafe { &*pose }))
        };
        s.backend.set_hand(hand, pose);
    }

    /// Convert a C-ABI [`IdtxHandPose`] into the neutral [`HandPose`] the XR
    /// stack consumes. Positions/orientations are copied verbatim (STAGE space);
    /// a non-positive radius falls back to 5 mm.
    fn to_hand_pose(h: &IdtxHandPose) -> HandPose {
        let mut out = HandPose::zeroed();
        for (i, j) in h.joints.iter().enumerate() {
            out.joints[i] = HandJoint {
                position: glam::Vec3::from_array(j.position),
                orientation: glam::Quat::from_array(j.orientation),
                radius: if j.radius > 0.0 { j.radius } else { 0.005 },
            };
        }
        out
    }

    /// Subtract the play-space `recenter` ground offset from an [`Xform`]'s
    /// translation (rotation + scale untouched) — the per-pose half of the
    /// recenter, matching the `OpenXR` feed.
    fn recenter_compensate(xform: Xform, recenter: glam::Vec3) -> Xform {
        Xform {
            translation: (glam::Vec3::from(xform.translation) - recenter).into(),
            ..xform
        }
    }

    /// Convert a [`HandPose`] to the 26-element [`XrHandJointResult`] array the
    /// behavior ABI expects, subtracting `recenter` from every joint position
    /// (the hand skeleton follows the body re-base, like the grip/aim poses).
    /// Mirrors the session's `OpenXR`-side `hand_pose_to_wire`.
    fn hand_pose_to_wire(
        pose: &HandPose,
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

    /// Append a diagnostic line to `idtx_visionos.log`; see
    /// [`super::visionos_diag`] for the location + pull command. Thin alias kept
    /// so the immersive module's many call sites stay terse.
    fn diag(msg: &str) {
        super::visionos_diag(msg);
    }

    /// Apply the same validated bootstrap snapshot to every session. Failed
    /// manifest loads produce a denied snapshot; later level opens do not reread
    /// configuration and diverge from the asset resolver's policy.
    #[cfg(any(feature = "remote", feature = "visionos-immersive"))]
    fn apply_bundle_network_policy(
        session: &mut StudioSession,
        manifest: &idtx_runtime_manifest::RuntimeManifest,
    ) {
        if let Err(error) = session.set_auth_origins(&manifest.network.auth_origins) {
            diag(&format!("credential origins rejected: {error}"));
        }
        session.set_exec_network_policy(manifest.network.egress_policy());
    }

    /// Find the bundle root — the directory holding `run.toml` — for the stage
    /// path the Swift shell handed us. The shell may pass the flat bundle-root
    /// stage (`<bundle>/stage.usdz`) OR, for a multi-level experience, the
    /// resolved start level (`<bundle>/levels/<id>/level.usdz`). Walk up from the
    /// stage's directory (bounded) until `run.toml` is found, so plugin
    /// discovery, schema/exec registration, the network policy, AND the levels
    /// list all anchor to the real bundle rather than a nested level dir — a
    /// wrong anchor breaks USD plugin discovery with a `TF_FATAL`/SIGABRT at
    /// stage open. Falls back to the stage's own directory.
    fn resolve_bundle_dir(stage_path: &str) -> std::path::PathBuf {
        let start = std::path::Path::new(stage_path)
            .parent()
            .map_or_else(|| std::path::PathBuf::from("."), std::path::Path::to_path_buf);
        let mut dir = start.as_path();
        for _ in 0..4 {
            if dir.join("run.toml").is_file() {
                return dir.to_path_buf();
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => break,
            }
        }
        start
    }

    /// The stage file to open: the shell-handed `stage_path` when it exists, else
    /// the bundle's `[experience].start_level` (a multi-level bundle has no flat
    /// `stage.usdz`, so a shell that hard-codes `stage.usdz` hands a nominal path
    /// that is not on disk). Returns `stage_path` unchanged when neither resolves,
    /// so the open surfaces a clean "could not open the root layer" rather than a
    /// silent empty-path failure.
    fn resolve_open_stage(
        stage_path: &str,
        bundle_dir: &std::path::Path,
        manifest: &idtx_runtime_manifest::RuntimeManifest,
    ) -> std::path::PathBuf {
        let given = std::path::Path::new(stage_path);
        if given.is_file() {
            return given.to_path_buf();
        }
        if let Some(rel) = manifest.experience.start_level.as_deref() {
            let anchored = if rel.is_relative() {
                bundle_dir.join(rel)
            } else {
                rel.to_path_buf()
            };
            if anchored.is_file() {
                diag(&format!(
                    "no stage at '{stage_path}'; booting start_level {}",
                    anchored.display()
                ));
                return anchored;
            }
        }
        given.to_path_buf()
    }

    /// Render one immersive stereo frame into the shell's drawable textures.
    ///
    /// Returns `0` on success, non-zero on error (null handle/frame, no eyes).
    /// The shell presents the drawable after this returns.
    ///
    /// # Safety
    ///
    /// `session` must be a handle from [`idtx_visionos_session_create`] not yet
    /// destroyed; `frame` a valid [`IdtxFrameInput`] whose `color_texture`s are
    /// the current drawable's live textures. Called on the shell's render thread.
    #[unsafe(no_mangle)]
    pub extern "C" fn idtx_visionos_render_frame(
        session: *mut VisionosSession,
        frame: *const IdtxFrameInput,
    ) -> i32 {
        if session.is_null() || frame.is_null() {
            return 1;
        }
        // SAFETY: caller contract — both pointers are live for the call and the
        // session handle is used only from this thread (so a &mut is unique).
        let session = unsafe { &mut *session };
        let frame = unsafe { &*frame };
        session.render(frame)
    }

    /// Destroy an immersive session, freeing its GPU + USD resources.
    ///
    /// # Safety
    ///
    /// `session` must be a handle from [`idtx_visionos_session_create`] that has
    /// not already been destroyed; it must not be used afterward.
    #[unsafe(no_mangle)]
    pub extern "C" fn idtx_visionos_session_destroy(session: *mut VisionosSession) {
        if session.is_null() {
            return;
        }
        // Flush the rolling per-eye render metrics to the app's `tmp/` container
        // before teardown, so even a short device run yields a pullable
        // `idtx-xr-metrics-<stage>-<ts>.json` (pull via `cargo xtask
        // pull-visionos-metrics`). No-op unless built with `--features metrics`.
        // Select the rendered level or transfer session, including after a swap.
        #[cfg(feature = "metrics")]
        // SAFETY: caller contract — `session` is a live, not-yet-destroyed handle;
        // we only borrow it here and consume it in the `Box::from_raw` below.
        unsafe {
            (*session).sessions.active_session().visionos_flush_metrics();
        }
        // SAFETY: caller contract — a live handle, consumed exactly once.
        drop(unsafe { Box::from_raw(session) });
        tracing::info!("idtx-runtime: visionOS immersive session destroyed");
    }

    /// Bring up wgpu-on-Metal, open the stage, and attach Pulley behaviors.
    fn build_session(path: &str, mtl_device: *mut c_void) -> Result<VisionosSession, String> {
        diag(&format!("build_session start, stage={path}"));

        // Resolve the bundle root (the dir with `run.toml`) and the actual stage
        // to open. The Swift shell may hand us the flat `<bundle>/stage.usdz` or,
        // for a multi-level experience, the resolved `<bundle>/levels/<id>/
        // level.usdz` (its start level) — either way anchor everything below to
        // the real bundle, and boot `start_level` when no flat stage exists.
        let bundle_dir = resolve_bundle_dir(path);
        let manifest = std::sync::Arc::new(idtx_usd_resolver_http::configure_network(
            || idtx_runtime_manifest::RuntimeManifest::load(bundle_dir.join("run.toml")),
            |manifest| manifest.network.asset_policy(),
        ).unwrap_or_else(|error| {
            diag(&format!("no/invalid run.toml ({error}); all network access denied"));
            idtx_runtime_manifest::RuntimeManifest::default()
        }));
        let open_stage = resolve_open_stage(path, &bundle_dir, &manifest);
        diag(&format!(
            "resolved bundle={} open_stage={}",
            bundle_dir.display(),
            open_stage.display()
        ));

        // 0a. Install the HTTP(S) USD resolver + arm its bearer token / host
        //     allow-list FIRST — before any other USD/Plug/Ar activity (the schema
        //     + exec plugin registration below, and the stage open) — so the
        //     http/https URI resolver is registered before Ar initializes and is
        //     picked up when the stage's remote references are composed. Mirrors the
        //     desktop shell / `run_android`, which install the resolver as the first
        //     USD-related call; this winit-free visionOS runtime uses neither.
        //
        //     Cache dir MUST be writable: `$HOME` (the sandbox root) is read-only on
        //     device, so the shared `~/.idtx-studio/usd-cache` fails `create_dir_all`
        //     with EPERM and the resolver never installs (the "Operation not
        //     permitted" bring-up failure). Fall back to the app's writable temp dir
        //     (`TMPDIR` → `<container>/tmp`, same as the diag log). The bearer token
        //     (no keychain on device) comes from run.toml's TEMPORARY
        //     `[network] dev_bearer_token`; see
        //     docs/plan_remote_refs_external_and_runtime_auth.md.
        {
            let cache_dir = idtx_remote::cache_dir()
                .ok()
                .filter(|d| std::fs::create_dir_all(d).is_ok())
                .unwrap_or_else(|| std::env::temp_dir().join("idtx-usd-cache"));
            match idtx_usd_resolver_http::install(&cache_dir) {
                Ok(()) => {
                    diag(&format!("HTTP USD resolver installed (cache {})", cache_dir.display()))
                }
                Err(e) => diag(&format!("HTTP USD resolver install failed ({e})")),
            }
            idtx_remote::auth::install_shared_refresh();
            if let Some(token) = manifest.network_dev_bearer_token() {
                diag(
                    "TEMPORARY: arming embedded run.toml bearer token for remote USD \
                     fetches — this build carries credentials; remove with the \
                     interactive logon-level flow",
                );
                if let Err(error) = idtx_auth::set_bearer_token_for_origins(
                    &manifest.network.auth_origins, token.to_owned(),
                ) {
                    diag(&format!("embedded credential not armed: {error}"));
                }
            }
        }

        // 0. Point OpenUSD plugin discovery at the bundled plugInfo trees BEFORE
        //    the first stage open. The winit-free visionOS runtime does NOT use
        //    `idtx-runtime-core` (which normally calls `configure_plugin_root`),
        //    so otherwise `register_plugins` falls back to the compile-time
        //    `layout::USD_DIR` — a build-host path that does not exist on device.
        //    With no file-format plugins registered, `UsdStage::Open` on a `.usdz`
        //    hits a `TF_FATAL` and aborts (SIGABRT) before returning — an
        //    immediate crash with no Rust panic. The packager stages the trees at
        //    `<bundle>/Frameworks/usd/{lib,plugin}/usd`, and `register_plugins`
        //    expects `<root>/lib/usd` + `<root>/plugin/usd`, so the bundle's
        //    `Frameworks/usd` is exactly the root to configure. (The cooked stage
        //    sits at the bundle root, so its parent is the bundle.) Best-effort:
        //    a re-entry returns "already configured", which we log and ignore.
        {
            let bundle = bundle_dir.as_path();
            let usd_root = bundle.join("Frameworks").join("usd");
            match idtx_usd::configure_plugin_root(&usd_root) {
                Ok(()) => diag(&format!("configured USD plugin root: {}", usd_root.display())),
                Err(e) => diag(&format!("configure_plugin_root skipped ({e})")),
            }

            // 0b. Register the IDTX codeless schema (`IdtxCollisionAPI` + addenda)
            //     so `prim_has_api("IdtxCollisionAPI")` resolves a `TfType` and
            //     collision bodies hydrate. Without it the schema is unknown,
            //     `convexHull` colliders with pre-baked hulls are dropped as
            //     non-colliding, and collidable-only raycasts (the HMD gaze
            //     highlight, grab picking) find nothing on device. Desktop does
            //     this via the shell's `install()`, Android via
            //     `idtx-runtime-core::register_idtx_schemas`; this winit-free
            //     runtime uses neither, so register the resources the packager
            //     staged at `<bundle>/usd-schemas/` directly. Idempotent + alias-
            //     safe (see `idtx_usd_extension::install_at`). Best-effort: a
            //     failure only means collision queries return no hits (logged).
            let schema_dir = bundle.join("usd-schemas");
            if schema_dir.join("plugInfo.json").is_file() {
                match idtx_usd_extension::install_at(&schema_dir) {
                    Ok(()) => diag(&format!(
                        "registered IDTX schema plugin: {}",
                        schema_dir.display()
                    )),
                    Err(e) => diag(&format!("IDTX schema registration failed ({e})")),
                }
            } else {
                diag(&format!(
                    "IDTX schema dir absent (collision will not hydrate): {}",
                    schema_dir.display()
                ));
            }

            // 0c. Register the coded OpenExec plugin (feature `openexec`) from the
            //     bundle's `usd-exec/` dir (staged there by the packager from the
            //     cross-built plugin). Desktop/Android do this via
            //     `idtx-runtime-core::register_idtx_schemas`; this winit-free
            //     visionOS runtime uses neither, so register it directly — a mirror
            //     of the codeless-schema block above. Best-effort: without it,
            //     authored compute graphs build value keys but find no computation.
            #[cfg(feature = "openexec")]
            {
                let exec_dir = bundle.join("usd-exec");
                if exec_dir.join("plugInfo.json").is_file() {
                    match idtx_usd_exec::install_at(&exec_dir) {
                        Ok(()) => {
                            diag(&format!("registered IDTX OpenExec plugin: {}", exec_dir.display()))
                        }
                        Err(e) => diag(&format!("IDTX OpenExec registration failed ({e})")),
                    }
                } else {
                    diag(&format!(
                        "IDTX OpenExec plugin dir absent (compute graphs inert): {}",
                        exec_dir.display()
                    ));
                }
            }
        }

        let bundle_context = idtx_session_orchestration::BundleContext::new(bundle_dir.clone());
        bundle_context.install_asset_search_path(&[]).map_err(|error| error.to_string())?;

        // 1. Surfaceless wgpu Metal device. On single-GPU Vision Pro this is the
        //    system-default MTLDevice — the same object the shell's LayerRenderer
        //    uses, so its drawable textures import into this device.
        let gpu = pollster::block_on(DesktopGpu::new_metal_offscreen(1, 1))
            .map_err(|e| format!("Metal device bring-up: {e}"))?;
        diag("metal device ready");

        // Verify the shared-device invariant (warn, don't fail: the Simulator may
        // report a distinct wrapper object even on one GPU).
        if let Some(h) = gpu.metal_handles() {
            if !mtl_device.is_null() && h.device != mtl_device {
                tracing::warn!(
                    "wgpu Metal device ({:p}) differs from the CompositorServices device ({:p}); \
                     drawable import relies on a single-GPU system",
                    h.device,
                    mtl_device
                );
            }
        }

        // 2. Open the stage on the shared device. A 1×1 viewport placeholder — the
        //    per-eye targets are sized to the drawable in `visionos_render_frame`.
        //    Texture-decode profile comes from the cooked manifest's
        //    `[experience].texture_quality` (so the project's [build.visionos]
        //    setting governs on-device texture residency); an unset/unknown token
        //    falls back to the capped `AUTHORING` default rather than full-res, so
        //    large textures do not exhaust the per-prefetch upload budget.
        let texture_quality = manifest
            .experience
            .texture_quality
            .as_deref()
            .and_then(idtx_studio_session::TextureDecodeConfig::from_profile_token)
            .unwrap_or_default();
        let session = StudioSession::open_with_gpu(
            &open_stage,
            gpu.device.clone(),
            gpu.queue.clone(),
            [1, 1],
            texture_quality,
            idtx_studio_session::RenderWorldUnitScale::default(),
        )
        .map_err(|e| format!("open stage: {e}"))?;
        diag("stage opened");

        // 3. Pulley behavior host (visionOS forbids the Cranelift JIT; the
        //    `visionos` feature switches wasmtime to the Pulley interpreter).
        //
        //    Seed the host with the stage's input map UP FRONT — `BehaviorHost::
        //    attach` freezes each behavior's action-id table from the router AT
        //    ATTACH TIME (host.rs `attach`), so a router installed *after* attach
        //    leaves `right_grab`/`left_grab` resolving to `UNRESOLVED` and no
        //    pinch can ever grab (the exact bug that made hands-free grab a no-op,
        //    even though the gesture sources fire). Same ordering rule as the
        //    scene backend below.
        let mut session = session;

        // Apply the bundle's `[network]` policy to stage-authored OpenExec REST
        // datasources. `StudioSession` is deny-by-default, and this winit-free
        // path has no `idtx-runtime-core` bootstrap to resolve `run.toml` and
        // call `set_exec_network_policy` — so without this every `RestDatasource`
        // stays blocked on visionOS even when the packaged manifest enables and
        // allowlists it. Must run before the first `tick_exec` (the policy takes
        // effect on the next exec rebuild).
        #[cfg(any(feature = "remote", feature = "visionos-immersive"))]
        apply_bundle_network_policy(&mut session, &manifest);

        // The shared bootstrap installs input/backend policy before attachment.
        // The session-owned host uses Pulley through the visionOS feature.
        let attached_behaviors = bundle_context.configure_session(&mut session);
        let locomotion_attached = session.has_live_behavior(idtx_studio_session::xr_controller_prefab::PLAYER_ROOT_PATH);
        tracing::info!(
            attached = attached_behaviors,
            locomotion_attached,
            "idtx-runtime: attached authored Pulley behaviors"
        );

        // One-shot collision-hydration proof: with the IDTX schema now registered,
        // collidable prims should carry an enabled CollisionComp. `enabled=0` means
        // collision never hydrated (so the gaze raycast can never hit); `pickable=0`
        // means the pick broad phase has no candidates at all.
        let (col_enabled, col_total, pickable) = session.collision_debug_counts();
        diag(&format!(
            "collision hydration: {col_enabled} enabled / {col_total} total collider(s), {pickable} pickable prim(s)"
        ));

        // Bind the immersive viewer to the injected player rig's camera when the
        // cooked stage composed one (an `xr_standalone` player overlay authors
        // `/World/Player/Camera` with playerRole=local). `visionos_render_frame`
        // then anchors the ARKit head pose onto that camera's live world transform
        // ("player rig + headset offset"), so the viewer spawns at the rig and a
        // locomotion behavior moving the rig moves the viewer — instead of the
        // AABB-centring placement used when no player camera exists.
        if let Some(path) = session.local_player_camera_path() {
            tracing::info!(camera = %path, "idtx-runtime: visionOS viewer bound to local player camera");
            session.use_scene_camera(path);
            // Guardrail: the eye path subtracts the play-space recenter every
            // frame; the pawn-root `xr_player_move` behavior is what bakes the
            // matching `R(yaw)·recenter` back into the root so the two cancel. If
            // a local-player camera exists but that root behavior did NOT attach,
            // subtracting a recenter nothing counter-bakes would make the world
            // drift with the head. Surface it loudly (the render loop forces
            // `recenter = ZERO` in this case — a stable, non-drifting view).
            if !locomotion_attached {
                diag(
                    "WARNING: local player camera present but pawn-root locomotion \
                     behavior (/World/Player) is NOT attached — recenter disabled to \
                     avoid the world tracking the head. Check earlier 'attach failed' \
                     lines (e.g. wasm address-space exhaustion).",
                );
            }
        } else {
            tracing::info!(
                "idtx-runtime: no local player camera in stage; visionOS viewer uses AABB placement"
            );
        }

        // Spawn the background hand-gesture recogniser (pinch/fist/…). Driven
        // from the ARKit hand poses each frame; its `Gesture/<kind>/<hand>`
        // events are pressed onto the frame input so behaviors read the same
        // gesture actions as on Quest/PCVR.
        #[cfg(feature = "gesture-recognition")]
        let gesture = {
            diag("gesture recognition: ENABLED (worker spawned)");
            Some(GestureWorker::spawn(GestureConfig::default()))
        };
        #[cfg(not(feature = "gesture-recognition"))]
        diag(
            "gesture recognition: DISABLED — this runtime dylib was built WITHOUT \
             the gesture-recognition feature; pinch gestures cannot fire. Rebuild \
             with `cargo xtask build-visionos-runtime` (enables it by default).",
        );

        tracing::info!(stage = %path, "idtx-runtime: visionOS immersive session open");

        // Level-transition wiring — parity with the desktop/Quest player, which
        // builds the SessionSet + TransitionDriver in `idtx-runtime-core`. This
        // winit-free runtime has no such bootstrap, so read the per-level assembly
        // list + transfer scene from the bundle's `run.toml` and build the driver
        // directly over the shared `idtx-session-orchestration` state machine. A
        // single-level bundle declares neither, so no transfer opens and no driver
        // is built — the set stays permanently on the one level (unchanged path).
        // No XR controller `Model` injection layer is carried: visionOS has no
        // tracked controllers (the OpenXR paths pass the rig's controller layer so
        // swapped-in levels keep controller geometry; here that would be dead).
        let (levels, transfer_level): (Vec<std::path::PathBuf>, Option<std::path::PathBuf>) = {
            let b = bundle_dir.as_path();
            let anchor = |rel: &std::path::Path| -> std::path::PathBuf {
                if rel.is_relative() {
                    b.join(rel)
                } else {
                    rel.to_path_buf()
                }
            };
            let levels = manifest.experience.levels.iter().map(|p| anchor(p)).collect();
            let transfer = manifest.experience.transfer_level.as_deref().map(anchor);
            (levels, transfer)
        };

        // Open the resident transfer/mask session (if declared). It gets the
        // bundle network policy; its behaviors (if any) attach to the runtime host
        // only when the driver cuts to it (reattach-on-swap in the render loop).
        let transfer_session = transfer_level.as_ref().and_then(|tp| {
            match StudioSession::open_with_gpu(
                tp,
                gpu.device.clone(),
                gpu.queue.clone(),
                [1, 1],
                texture_quality,
                idtx_studio_session::RenderWorldUnitScale::default(),
            ) {
                Ok(mut s) => {
                    #[cfg(any(feature = "remote", feature = "visionos-immersive"))]
                    apply_bundle_network_policy(&mut s, &manifest);
                    bundle_context.configure_session(&mut s);
                    diag(&format!("transfer/mask session opened: {}", tp.display()));
                    Some(s)
                }
                Err(e) => {
                    diag(&format!("transfer session open failed ({e}); masking disabled"));
                    None
                }
            }
        });

        let driver = (!levels.is_empty()).then(|| {
            diag(&format!(
                "level-transition driver armed: {} level(s)",
                levels.len()
            ));
            let manifest = manifest.clone();
            let bundle_context = bundle_context.clone();
            let state = bundle_context.state.clone();
            TransitionDriver::new(
                LevelTable::new(levels),
                gpu.device.clone(),
                gpu.queue.clone(),
                [1, 1],
                Box::new(move |s: &mut StudioSession| {
                    // Every session receives the validated startup policy and
                    // the same session-owned input/behavior/camera bootstrap.
                    #[cfg(any(feature = "remote", feature = "visionos-immersive"))]
                    apply_bundle_network_policy(s, &manifest);
                    bundle_context.configure_session(s);
                }),
                String::new(),
            ).with_state(state)
        });

        let sessions = SessionSet::new(session, transfer_session);
        Ok(VisionosSession {
            sessions,
            driver,
            backend: CompositorServicesBackend::new(),
            frame_input: FrameInput::new(),
            #[cfg(feature = "gesture-recognition")]
            gesture,
            #[cfg(feature = "gesture-recognition")]
            gesture_prev: std::collections::HashSet::new(),
            #[cfg(feature = "gesture-recognition")]
            gesture_frame: 0,
            #[cfg(feature = "gesture-recognition")]
            diag_hands: (false, false),
            #[cfg(feature = "gesture-recognition")]
            diag_point: (false, false),
            frame_count: 0,
            presentation_queue_shared: false,
            #[cfg(feature = "metrics")]
            frame_metrics: idtx_session_orchestration::frame_metrics::FrameMetrics::new("visionos-compositor", 11_111_111),
            _gpu: gpu,
        })
    }

    impl VisionosSession {
        /// Forward this frame's hand poses to the gesture worker, drain the
        /// recognised events, and press the active `Gesture/<kind>/<hand>`
        /// sources onto the frame input (releasing the ones that ended) — the
        /// visionOS counterpart of `XrPreviewSession::pump_gestures`. The caller
        /// then hands `frame_input` to `update_input` (one input feed per frame).
        /// The active set evolves from the previous frame so a frame carrying no
        /// event keeps a held gesture down rather than flickering.
        #[cfg(feature = "gesture-recognition")]
        fn pump_gestures(
            &mut self,
            left: Option<&HandPose>,
            right: Option<&HandPose>,
            timestamp: f64,
        ) {
            // Diagnostic: hand-tracking presence (None↔Some), logged once per
            // transition. If this never logs `left=true`/`right=true`, the Swift
            // shell is not feeding joints (hand tracking unauthorised / shell not
            // calling idtx_visionos_session_set_hand) → no pinch is possible.
            let present = (left.is_some(), right.is_some());
            if present != self.diag_hands {
                diag(&format!(
                    "hand tracking presence: left={} right={}",
                    present.0, present.1
                ));
                self.diag_hands = present;
            }

            let Some(link) = self.gesture.as_ref() else {
                return;
            };
            // Stamp the frame time so the recogniser's temporal detectors key off
            // a monotonic clock (pinch is single-frame; swipes/holds need it).
            let stamp = |p: Option<&HandPose>| {
                p.map(|p| {
                    let mut p = p.clone();
                    p.timestamp = timestamp;
                    p
                })
            };
            let snap = HandSnapshot {
                left: stamp(left),
                right: stamp(right),
                frame_index: self.gesture_frame,
            };
            self.gesture_frame = self.gesture_frame.wrapping_add(1);
            let _ = link.send(snap);

            // Point-pose diagnostic (teleport aim gesture): log the measured
            // finger distances vs thresholds whenever the point/no-point verdict
            // flips, and ~once/sec while a hand is present, so a field session can
            // see *why* a teleport point did or did not register and retune the
            // thresholds. The recogniser runs with `GestureConfig::default()`, so
            // the diagnostic uses the matching `SimpleGestureConfig::default()`.
            {
                let cfg = SimpleGestureConfig::default();
                let left_d = left.map(|p| point_diagnostics(p, &cfg));
                let right_d = right.map(|p| point_diagnostics(p, &cfg));
                let verdict = (
                    left_d.is_some_and(|d| d.is_point()),
                    right_d.is_some_and(|d| d.is_point()),
                );
                // ~1 Hz at 90 fps, plus an immediate log on any verdict flip.
                let periodic = self.gesture_frame % 90 == 0;
                if verdict != self.diag_point || periodic {
                    // Include the absolute index-tip position (stage space, as fed
                    // to behaviors) so a cue-vs-passthrough offset can be checked
                    // against the hand frame — joint 10 is the index tip — and the
                    // thumb↔index cosine so the thumb-snap commit can be tuned
                    // (~0 cocked up, ~1 snapped alongside the index).
                    if let (Some(d), Some(p)) = (left_d, left) {
                        let t = p.joints[10].position;
                        let tc = idtx_platform_xr::thumb_index_cos(p).unwrap_or(f32::NAN);
                        let gap = thumb_index_gap(p);
                        diag(&format!(
                            "point L: {d} tip=({:.2},{:.2},{:.2}) thumbCos={:.2} gap={:.3}",
                            t.x, t.y, t.z, tc, gap
                        ));
                    }
                    if let (Some(d), Some(p)) = (right_d, right) {
                        let t = p.joints[10].position;
                        let tc = idtx_platform_xr::thumb_index_cos(p).unwrap_or(f32::NAN);
                        let gap = thumb_index_gap(p);
                        diag(&format!(
                            "point R: {d} tip=({:.2},{:.2},{:.2}) thumbCos={:.2} gap={:.3}",
                            t.x, t.y, t.z, tc, gap
                        ));
                    }
                    self.diag_point = verdict;
                }
            }

            let events = link.drain();
            let mut active = self.gesture_prev.clone();
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
            // Diagnostic: the recognised gesture-source set, logged once per change
            // (e.g. a pinch adds `Gesture/pinch/right`). If this logs the source but
            // nothing is grabbed, the break is downstream — the input_map binding or
            // the behavior; if it never logs on a pinch, the recogniser is the break.
            if active != self.gesture_prev {
                let mut names: Vec<&str> = active.iter().map(String::as_str).collect();
                names.sort_unstable();
                diag(&format!("gesture sources active: {names:?}"));
            }
            for source in &active {
                self.frame_input.press(source.as_str());
            }
            for source in self.gesture_prev.difference(&active) {
                self.frame_input.release(source);
            }
            self.gesture_prev = active;
        }

        /// Feed poses to the backend, advance behaviors + animation, render both
        /// eyes into the drawable textures.
        fn render(&mut self, frame: &IdtxFrameInput) -> i32 {
            #[cfg(feature = "metrics")]
            self.frame_metrics.configure(self.sessions.active_session().source_path(), [frame.width, frame.height],
                idtx_studio_session::visionos_render::RENDER_SCALE, self._gpu.device.features(),
                &format!("metrics remote={} openexec={} gesture={}", cfg!(feature = "remote"), cfg!(feature = "openexec"), cfg!(feature = "gesture-recognition")));
            let view_count = (frame.view_count as usize).min(MAX_EYES);
            if view_count == 0 {
                return 2;
            }

            // 1. Head + per-eye poses → the XR backend (the per-frame XR state the
            //    renderer and behaviors read through the `XrBackend` trait).
            let head = HmdPose {
                position: glam::Vec3::from(frame.head_pos),
                orientation: quat(frame.head_orientation),
                timestamp: frame.timestamp,
            };
            self.backend.set_frame(
                head,
                eye_input(&frame.eyes[0], frame.timestamp),
                eye_input(&frame.eyes[1], frame.timestamp),
            );

            // Play-space recenter — the visionOS capsule-follow, driven by the
            // authored behaviors at parity with the OpenXR path. Compute the
            // offset (5 cm hysteresis) from the ARKit head ground position and
            // feed it to BOTH the session (so `visionos_render_frame` subtracts it
            // from each eye) and the behavior host (so `xr_player_move` bakes
            // `R(yaw)·recenter` into the pawn root). The two cancel for the view —
            // room-scale walking is preserved — while the pawn/capsule re-bases
            // under the head. `author_xr_pawn_recenter` is gone: the bake is the
            // behavior's job now (step 2).
            //
            // Gate on `locomotion_attached`: the eye-side subtraction and the
            // root-side bake are the two halves of one effect and must act
            // together. If the pawn-root `xr_player_move` behavior is not attached
            // (so nothing bakes the offset), a non-zero recenter would be
            // subtracted with no counter-bake and the world would drift with the
            // head. Forcing `ZERO` keeps the view stable in that failure mode.
            let prev_recenter = self.sessions.active_session_mut().xr_recenter();
            let has_player = self.sessions.active_session_mut().local_player_camera_path().is_some();
            let recenter = if self.sessions.active_session().has_live_behavior(idtx_studio_session::xr_controller_prefab::PLAYER_ROOT_PATH)
                && has_player
            {
                idtx_studio_session::recenter_step(
                    prev_recenter,
                    idtx_studio_session::flatten_ground(glam::Vec3::from(frame.head_pos)),
                    idtx_studio_session::RECENTER_THRESHOLD,
                )
            } else if has_player {
                prev_recenter
            } else {
                glam::Vec3::ZERO
            };
            self.sessions.active_session_mut().set_xr_recenter(recenter);

            // Feed the HMD pose AND the located hand skeletons into the host so
            // behaviors read the same input as on Quest/PCVR — parity with the
            // OpenXR `feed_xr_snapshot`. Poses are in stage space with the
            // recenter subtracted (matching the OpenXR feed), so a behavior reads
            // them relative to the recentered origin. In hands-free mode (no
            // controllers) the grip/aim spaces fall back to the hand (WRIST →
            // grip, PALM → aim), so controller-oriented behaviors work too.
            let head_xform = Xform::from_trs(
                glam::Vec3::from(frame.head_pos) - recenter,
                quat(frame.head_orientation),
                glam::Vec3::ONE,
            );
            let left_hand = self.backend.hand_pose(Hand::Left);
            let right_hand = self.backend.hand_pose(Hand::Right);
            let mut snap = XrSnapshot::with_hmd(head_xform);
            for (side, grip_space, aim_space, hand) in [
                (
                    XrSide::Left,
                    XrSpace::LeftHand,
                    XrSpace::LeftAim,
                    left_hand.as_ref(),
                ),
                (
                    XrSide::Right,
                    XrSpace::RightHand,
                    XrSpace::RightAim,
                    right_hand.as_ref(),
                ),
            ] {
                if let Some(p) = hand {
                    snap.set_hand_joints(side, Some(hand_pose_to_wire(p, recenter)));
                    // visionOS is hand-tracking only — it never surfaces
                    // controllers. Report HAND so `ctx.xr().input_source(side)`
                    // and `has_hand()` work in parity with the OpenXR feed
                    // (where `classify_input_source` sets this). Without it the
                    // side stays at the default `NONE`, so behaviours gated on
                    // `has_hand()` bail out every frame on device.
                    snap.set_input_source(side, XrInputSource::HAND);
                    let wrist = p.joints[1]; // WRIST → grip space
                    snap.set(
                        grip_space,
                        XrPoseResult::tracked(recenter_compensate(
                            Xform::from_trs(wrist.position, wrist.orientation, glam::Vec3::ONE),
                            recenter,
                        )),
                    );
                    let palm = p.joints[0]; // PALM → aim space
                    snap.set(
                        aim_space,
                        XrPoseResult::tracked(recenter_compensate(
                            Xform::from_trs(palm.position, palm.orientation, glam::Vec3::ONE),
                            recenter,
                        )),
                    );
                }
            }

            // 2. Advance stage animation BEFORE behaviors, matching the canonical
            //    Quest/PCVR frame loop (animation → behaviors → physics in
            //    `idtx-runtime-core`'s `player::frame`). Running animation first
            //    means a behavior reading an animated prim this frame sees the
            //    current pose, not last frame's, and preserves the one-frame
            //    transport settle `tick_behaviors` documents.
            if let Err(e) = self.sessions.active_session_mut().tick_animation(f64::from(frame.dt_seconds)) {
                tracing::warn!("visionOS tick_animation: {e}");
            }

            // Private-stage writes remain responsive while another level opens.
            {
                #[cfg(feature = "gesture-recognition")]
                self.pump_gestures(left_hand.as_ref(), right_hand.as_ref(), frame.timestamp);
                self.sessions.active_session_mut().tick_external_behaviors(frame.dt_seconds, snap, recenter, self.frame_input.raw());
                self.frame_input.end_frame();
                self.sessions.active_session_mut().mark_dirty_if_behaviors_moved_transforms();
                if has_player && !self.sessions.active_session().has_live_behavior(idtx_studio_session::xr_controller_prefab::PLAYER_ROOT_PATH) {
                    self.sessions.active_session_mut().set_xr_recenter(prev_recenter);
                }
            }

            // 3c. All commands have already been applied to their owning session
            // before polling a transition. Failure never rebuilds the retained host.
            if let Some(driver) = &mut self.driver {
                let width = frame.width.max(1);
                let height = frame.height.max(1);
                let format = wgpu_format_from_mtl(frame.color_pixel_format);
                driver.set_viewport([width, height]);
                driver.set_prepare(std::sync::Arc::new(move |session| session.prepare_visionos_resources(width, height, view_count, format)));
                // Each displaced session owns its host and tears it down against
                // its own world. An aborted transition retains the live host.
                drop(driver.poll(&mut self.sessions));
            }

            // 4. Physics control was drained by the owning session before swap.
            //    Step the `UsdPhysicsScene`-gated rigid-body sim against this frame's
            //    kinematic goals (set by animation + behaviors above) and author the
            //    moved dynamic set back into the session layer, before the render
            //    reads the result. A no-op unless the stage carries a
            //    `UsdPhysicsScene`. `defer_reprojection = false`: unlike the OpenXR
            //    path (whose `render_xr` drains the physics writes alongside the
            //    controller poses), this runtime has no such later drain, so
            //    `tick_physics` owns the sole drain + reprojection — matching the
            //    desktop/headless path.
            if let Err(e) = self.sessions.active_session_mut().tick_physics(frame.dt_seconds, false) {
                tracing::warn!("visionOS tick_physics: {e}");
            }

            // 3b. Drive the OpenExec compute graph: apply pending datasource
            //     payloads, Compute, and author changed results into the session
            //     layer before the render reads them. The plugin is registered at
            //     session-create, but — unlike the desktop/XR player loops in
            //     `idtx-runtime-core` — nothing here drives it, so without this call
            //     compute graphs and datasource responses stay inert on device.
            //     `defer_reprojection = false`: this runtime has no later drain.
            #[cfg(feature = "openexec")]
            if let Err(e) = self.sessions.active_session_mut().tick_exec(false) {
                tracing::warn!("visionOS tick_exec: {e}");
            }

            // 4. Render both eyes into the imported drawable textures.
            let StereoViews { left, right } = self.backend.stereo_views();
            // Capture the left eye's resolved frustum for the frame-1 diagnostic
            // (angles in radians; left/down should be negative, right/up positive).
            let lf = left.fov;
            let left_pos = left.pose.position;
            let eyes = [
                VisionosEyeFrame {
                    color_texture: frame.eyes[0].color_texture,
                    depth_texture: frame.eyes[0].depth_texture,
                    view: left,
                },
                VisionosEyeFrame {
                    color_texture: frame.eyes[1].color_texture,
                    depth_texture: frame.eyes[1].depth_texture,
                    view: right,
                },
            ];
            self.sessions.active_session_mut().visionos_render_frame(
                &eyes[..view_count],
                frame.width.max(1),
                frame.height.max(1),
                wgpu_format_from_mtl(frame.color_pixel_format),
            );
            // Generated shells present on our queue: Metal FIFO orders the
            // render/blit and present without a CPU wait. Legacy foreign shells
            // must retain the completion barrier until they adopt that contract.
            if !self.presentation_queue_shared {
                let _ = self._gpu.device.poll(wgpu::PollType::wait_indefinitely());
            }
            self.frame_count += 1;
            // One-shot + periodic proof the per-eye render path ran on-device. The
            // frame-1 line carries the real poses / tangents / resolved FOV +
            // drawable size so a black-scene bring-up can be diagnosed from the log
            // (degenerate FOV, off-origin head pose, or wrong tangent signs).
            if self.frame_count == 1 {
                let e0 = &frame.eyes[0];
                diag(&format!(
                    "frame1 views={view_count} {}x{} fmt={} head_pos={:?} eye0_pos={:?} \
                     eye0_tangents(l,r,u,d)={:?} left_fov_rad(l,r,u,d)=[{:.4},{:.4},{:.4},{:.4}] \
                     left_eye_pos={:?}",
                    frame.width,
                    frame.height,
                    frame.color_pixel_format,
                    frame.head_pos,
                    e0.pos,
                    e0.tangents,
                    lf.angle_left,
                    lf.angle_right,
                    lf.angle_up,
                    lf.angle_down,
                    left_pos,
                ));
            } else if self.frame_count == 120 {
                diag(&format!(
                    "rendered frame {} ({view_count} eyes, {}x{})",
                    self.frame_count, frame.width, frame.height
                ));
            }
            0
        }
    }

    /// Build an [`EyeViewInput`] from the C eye struct.
    fn eye_input(eye: &IdtxEyeInput, timestamp: f64) -> EyeViewInput {
        EyeViewInput {
            position: glam::Vec3::from(eye.pos),
            orientation: quat(eye.orientation),
            tan_left: eye.tangents[0],
            tan_right: eye.tangents[1],
            tan_up: eye.tangents[2],
            tan_down: eye.tangents[3],
            timestamp,
        }
    }

    fn quat(q: [f32; 4]) -> glam::Quat {
        glam::Quat::from_xyzw(q[0], q[1], q[2], q[3])
    }

    /// Map a Metal `MTLPixelFormat` raw value to the matching wgpu format for the
    /// drawable blit target. Defaults to `Rgba16Float` (the common visionOS HDR
    /// drawable format) for unrecognised values.
    fn wgpu_format_from_mtl(mtl: u32) -> wgpu::TextureFormat {
        match mtl {
            70 => wgpu::TextureFormat::Rgba8Unorm,     // MTLPixelFormatRGBA8Unorm
            71 => wgpu::TextureFormat::Rgba8UnormSrgb, // RGBA8Unorm_sRGB
            80 => wgpu::TextureFormat::Bgra8Unorm,     // BGRA8Unorm
            81 => wgpu::TextureFormat::Bgra8UnormSrgb, // BGRA8Unorm_sRGB
            115 => wgpu::TextureFormat::Rgba16Float,   // RGBA16Float
            other => {
                tracing::warn!("unmapped MTLPixelFormat {other}; defaulting to rgba16Float");
                wgpu::TextureFormat::Rgba16Float
            }
        }
    }
}
