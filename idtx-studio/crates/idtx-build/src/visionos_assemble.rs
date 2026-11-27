//! Shared, launchable visionOS `.app` assembly: the Swift `CompositorServices`
//! immersive shell, the dylib load-closure + rpath fixups, the development
//! entitlements plist, and the device finishing orchestrator
//! ([`finish_device_app`]) that compiles the shell, embeds the provisioning
//! profile, and signs for a physical Vision Pro.
//!
//! This lives in `idtx-build` (not `xtask`) so both the `cargo xtask
//! run-visionos-device` command and the Conductor "Build Vision Pro Experience"
//! menu drive the **same** assembler. The Swift source / C header / `swiftc`
//! arg-vector / dylib predicate are pure and unit-tested here (and reused by the
//! Simulator path in `xtask`); only the orchestration shells out, gated to macOS.

#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};

use crate::signing::{SigningConfig, SigningError};

/// Errors from launchable visionOS `.app` assembly.
#[derive(Debug, thiserror::Error)]
pub enum VisionOsAssembleError {
    /// Assembly orchestration is macOS-only.
    #[error("visionOS .app assembly is only available on macOS")]
    NotMacOs,
    /// An Apple command-line primitive failed.
    #[error(transparent)]
    AppleTool(#[from] crate::apple_tools::AppleToolError),
    /// Code-signing failed.
    #[error(transparent)]
    Signing(#[from] SigningError),
    /// A filesystem operation failed.
    #[error("visionOS assembly I/O at {path}: {source}")]
    Io {
        /// The path operated on.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// `swiftc` failed to compile the immersive shell.
    #[error("swiftc failed to compile the visionOS immersive shell: {0}")]
    Swiftc(String),
}

/// Whether `name` is a dynamic library we embed in `Frameworks/` (`*.dylib`,
/// including versioned names like `libtbb.12.dylib`).
#[must_use]
pub fn is_dylib_name(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|e| e == "dylib")
}

/// The development entitlements plist for a free-Personal-Team device build.
/// `application-identifier` + `com.apple.developer.team-identifier` must match the
/// embedded provisioning profile; `get-task-allow` lets the debugger attach (the
/// development profile grants it). ARKit usage strings live in Info.plist, NOT
/// here, so no ARKit entitlement is needed (and none is available to a free team).
#[must_use]
pub fn visionos_entitlements_plist(team_id: &str, bundle_id: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
\t<key>application-identifier</key><string>{team_id}.{bundle_id}</string>\n\
\t<key>com.apple.developer.team-identifier</key><string>{team_id}</string>\n\
\t<key>get-task-allow</key><true/>\n\
</dict>\n\
</plist>\n"
    )
}

/// `swiftc` arg vector compiling the immersive shell, linking
/// `libidtx_runtime.dylib` from `frameworks` and baking the bundle-relative rpath
/// so the loader finds the embedded dylibs. `clang_target` selects device
/// (`arm64-apple-xros<dep>`) vs simulator (`…-simulator`); `sdk_path` the matching
/// SDK.
#[must_use]
pub fn swiftc_args(
    sdk_path: &str,
    clang_target: &str,
    source: &Path,
    exe_out: &Path,
    frameworks: &Path,
    header: &Path,
) -> Vec<String> {
    vec![
        "-sdk".to_owned(),
        sdk_path.to_owned(),
        "-target".to_owned(),
        clang_target.to_owned(),
        // `@main` requires library-parse mode.
        "-parse-as-library".to_owned(),
        // Import the Rust immersive FFI C declarations for guaranteed layout parity.
        "-import-objc-header".to_owned(),
        header.display().to_string(),
        "-o".to_owned(),
        exe_out.display().to_string(),
        "-L".to_owned(),
        frameworks.display().to_string(),
        "-lidtx_runtime".to_owned(),
        "-Xlinker".to_owned(),
        "-rpath".to_owned(),
        "-Xlinker".to_owned(),
        "@executable_path/Frameworks".to_owned(),
        source.display().to_string(),
    ]
}

/// The C bridging header (`-import-objc-header`) declaring the Rust immersive FFI
/// — the `IdtxEyeInput`/`IdtxFrameInput`/`IdtxHandJoint`/`IdtxHandPose` structs
/// (layout-matched to the `#[repr(C)]` Rust types in `apps/idtx-runtime/src/lib.rs`)
/// and the four `idtx_visionos_*` entry points (create / set_hand / render_frame /
/// destroy).
#[must_use]
pub fn c_header() -> String {
    r"#pragma once
#include <stdint.h>
#include <stdbool.h>

typedef struct IdtxEyeInput {
    void *color_texture;
    void *depth_texture;
    float pos[3];
    float orientation[4];
    float tangents[4];
} IdtxEyeInput;

typedef struct IdtxFrameInput {
    float head_pos[3];
    float head_orientation[4];
    IdtxEyeInput eyes[2];
    uint32_t view_count;
    uint32_t width;
    uint32_t height;
    uint32_t color_pixel_format;
    float dt_seconds;
    double timestamp;
} IdtxFrameInput;

typedef struct IdtxHandJoint {
    float position[3];
    float radius;
    float orientation[4];
} IdtxHandJoint;

typedef struct IdtxHandPose {
    IdtxHandJoint joints[26];
} IdtxHandPose;

typedef struct VisionosSession VisionosSession;

VisionosSession *idtx_visionos_session_create(const char *stage_path, void *mtl_device);
void *idtx_visionos_command_queue(VisionosSession *session);
bool idtx_visionos_metrics_enabled(void);
void idtx_visionos_frame_complete(VisionosSession *session, uint64_t elapsed_ns);
void idtx_visionos_session_set_hand(VisionosSession *session, uint32_t side, const IdtxHandPose *pose);
int32_t idtx_visionos_render_frame(VisionosSession *session, const IdtxFrameInput *frame);
void idtx_visionos_session_destroy(VisionosSession *session);
"
    .to_owned()
}

/// The SwiftUI immersive-shell source. Opens an `ImmersiveSpace` whose
/// `CompositorLayer` drives a `CompositorServices` render loop: it creates the Rust
/// immersive session (handing it the Metal device + the bundled stage resource
/// `<stage_resource>.<stage_ext>`), then each frame queries the drawable, samples
/// the ARKit device anchor, builds an `IdtxFrameInput` with per-eye drawable
/// textures + poses, calls into Rust to render the stereo frame, and presents.
///
/// Device and Simulator share this source verbatim; only the `swiftc`
/// `-sdk`/`-target` differ (passed to [`swiftc_args`]).
#[must_use]
pub fn swift_shell_source(stage_resource: &str, stage_ext: &str, hand_tracking: bool) -> String {
    // Gate the ARKit `HandTrackingProvider` on the project's `enable_hand_tracking`
    // flag — the SAME flag that controls whether the packager emits
    // `NSHandsTrackingUsageDescription` in Info.plist (see `visionos_info_plist_with`).
    // If the shell ran hand tracking without that usage description, ARKit's
    // hand-tracking XPC service throws an uncaught NSException → `abort()` at launch.
    // Keeping both sides on ONE flag makes that mismatch impossible.
    let hand_tracking_swift = if hand_tracking { "true" } else { "false" };
    format!(
        r#"import SwiftUI
import CompositorServices
import Metal
import ARKit
import simd
import Foundation

// The Rust FFI (idtx_visionos_session_create / _render_frame / _session_destroy)
// and the IdtxFrameInput / IdtxEyeInput structs come from the imported C header.

// Append a breadcrumb to idtx_visionos.log in the app's writable temp dir — the
// SAME file the Rust side writes to (NSTemporaryDirectory() == TMPDIR ==
// std::env::temp_dir()). These Swift-side lines localize an "immediate crash"
// that happens BEFORE the Rust FFI is reached (dylib load, ARKit/CompositorServices
// bring-up, a missing bundled stage, or a force-unwrap). Pull after a crash with:
//   xcrun devicectl device copy from --device <UDID> \
//     --domain-type appDataContainer --domain-identifier <bundle-id> \
//     --source tmp/idtx_visionos.log --destination ./idtx_visionos.log --user mobile
func idtxDiag(_ msg: String) {{
    let path = (NSTemporaryDirectory() as NSString).appendingPathComponent("idtx_visionos.log")
    NSLog("IDTX_IMMERSIVE %@", msg)
    guard let data = "[swift \(String(format: "%.3f", Date().timeIntervalSince1970))] \(msg)\n".data(using: .utf8) else {{ return }}
    if let fh = FileHandle(forWritingAtPath: path) {{
        defer {{ try? fh.close() }}
        fh.seekToEndOfFile()
        fh.write(data)
    }} else {{
        try? data.write(to: URL(fileURLWithPath: path))
    }}
}}

struct IdtxLayerConfiguration: CompositorLayerConfiguration {{
    func makeConfiguration(capabilities: LayerRenderer.Capabilities,
                           configuration: inout LayerRenderer.Configuration) {{
        // The device compositor requires a valid depth format AND valid per-pixel
        // depth written each frame (used for reprojection); color-only content is
        // dropped (black) on device even though the Simulator renders it. We write
        // the drawable depth textures in the Rust render path.
        configuration.depthFormat = .depth32Float
        configuration.colorFormat = .rgba16Float
        configuration.isFoveationEnabled = false
        configuration.layout = .dedicated
    }}
}}

final class IdtxImmersiveRenderer {{
    // Compile-time gate from the project's `enable_hand_tracking` flag. When
    // false, the ARKit HandTrackingProvider is never created or run, so the app
    // needs no `NSHandsTrackingUsageDescription` and cannot abort on the ARKit
    // hand-tracking XPC service. Must stay in lock-step with the Info.plist key.
    static let handTrackingEnabled = {hand_tracking_swift}
    let layerRenderer: LayerRenderer
    let device: MTLDevice
    var commandQueue: MTLCommandQueue
    let arSession = ARKitSession()
    let worldTracking = WorldTrackingProvider()
    let handTracking = HandTrackingProvider()
    let stagePath: String
    // `session` is PUBLISHED once by the background build thread (see renderLoop)
    // and read by the render thread; `sessionLock` guards the handoff and
    // `sessionReady` marks the build complete (the pointer may be nil on failure).
    // `queueAdopted` is render-thread-only.
    var session: OpaquePointer?
    let sessionLock = NSLock()
    var sessionReady = false
    var queueAdopted = false
    var lastPresentationTime: Double?

    init(_ layerRenderer: LayerRenderer, stagePath: String) {{
        idtxDiag("renderer init (resolving MTLDevice + queue)")
        self.layerRenderer = layerRenderer
        self.device = MTLCreateSystemDefaultDevice()!
        self.commandQueue = self.device.makeCommandQueue()!
        self.stagePath = stagePath
        idtxDiag("renderer init done, stage=\(stagePath)")
    }}

    func start() {{
        idtxDiag("renderer start")
        Task {{
            var providers: [any DataProvider] = [worldTracking]
            if Self.handTrackingEnabled && HandTrackingProvider.isSupported {{
                providers.append(handTracking)
                idtxDiag("hand tracking supported; running")
            }} else {{
                idtxDiag("hand tracking NOT running (disabled for this build or unsupported); gestures off")
            }}
            do {{ try await arSession.run(providers); idtxDiag("ARKit providers running") }}
            catch {{ idtxDiag("ARKit run failed: \(error)") }}
        }}
        let thread = Thread {{ [weak self] in self?.renderLoop() }}
        thread.name = "idtx-render"
        thread.start()
    }}

    func renderLoop() {{
        // ================== CRITICAL: DO NOT BUILD THE SESSION HERE ==================
        // `idtx_visionos_session_create` brings up the wgpu Metal device, and
        // wgpu-core's `Queue::new` EAGERLY begins a Metal command buffer at device
        // creation (`pending_writes.activate()` -> `queue.commandBuffer()`). Running
        // that on THIS render thread, BEFORE the loop below has presented a frame,
        // DEADLOCKS on a real Vision Pro: the compositor advances only once we
        // present, the eager Metal command buffer completes only once the compositor
        // advances, and this thread is stuck inside session_create so it never
        // presents. The watchdog then kills the app after ~5 s (0x8BADF00D) — the
        // "deploys fine but won't launch" symptom.
        //
        // So: build the session on a BACKGROUND queue while this thread keeps
        // presenting empty (black) frames to drive the compositor; `renderFrame`
        // swaps in real rendering the first frame `currentSession()` is non-nil.
        // This off-thread handoff did NOT exist in earlier revisions — the inline
        // build was a latent trap that only began firing with heavier scene init +
        // newer wgpu's eager command-buffer-at-creation. DO NOT "simplify" this back
        // to an inline `session = idtx_visionos_session_create(...)` on this thread;
        // the test `swift_shell_builds_session_off_the_render_thread` guards it.
        // ============================================================================
        idtxDiag("renderLoop entered; launching background session build for \(stagePath)")
        let devPtr = UnsafeMutableRawPointer(Unmanaged.passUnretained(device).toOpaque())
        DispatchQueue.global(qos: .userInitiated).async {{ [weak self] in
            guard let self else {{ return }}
            var created: OpaquePointer?
            self.stagePath.withCString {{ cstr in
                created = idtx_visionos_session_create(cstr, devPtr)
            }}
            self.sessionLock.lock()
            self.session = created
            self.sessionReady = true
            self.sessionLock.unlock()
            idtxDiag(created == nil
                ? "session create FAILED for \(self.stagePath)"
                : "session created (background); render loop swapping it in")
        }}
        defer {{
            self.sessionLock.lock()
            let built = self.session
            self.session = nil
            self.sessionLock.unlock()
            if let built {{ idtx_visionos_session_destroy(built) }}
        }}
        while true {{
            switch layerRenderer.state {{
            case .paused:
                lastPresentationTime = nil
                if idtx_visionos_metrics_enabled(), let s = currentSession() {{ idtx_visionos_frame_complete(s, 0) }}
                layerRenderer.waitUntilRunning()
            case .running:
                autoreleasepool {{ renderFrame() }}
            case .invalidated:
                idtxDiag("layer invalidated; tearing down")
                return
            @unknown default:
                return
            }}
        }}
    }}

    /// The session pointer once the background build has published it, else nil.
    /// Cheap (uncontended lock); the build publishes exactly once.
    func currentSession() -> OpaquePointer? {{
        sessionLock.lock()
        defer {{ sessionLock.unlock() }}
        return sessionReady ? session : nil
    }}

    func renderFrame() {{
        // The session builds on a background thread (see renderLoop). Until it is
        // published, present empty frames so the compositor keeps advancing — that
        // is exactly what unblocks the background device bring-up.
        let sess = currentSession()
        if let sess, !queueAdopted {{
            // First frame the session is ready: adopt wgpu's command queue (created
            // on the shared Metal device), replacing the bootstrap queue the warmup
            // frames used.
            if let queuePtr = idtx_visionos_command_queue(sess) {{
                commandQueue = Unmanaged<AnyObject>.fromOpaque(queuePtr).takeUnretainedValue() as! MTLCommandQueue
                queueAdopted = true
                idtxDiag("render queue adopted from session")
            }}
        }}
        let metricsStart = (idtx_visionos_metrics_enabled() && sess != nil) ? DispatchTime.now().uptimeNanoseconds : 0
        defer {{
            if metricsStart != 0, let sess {{ idtx_visionos_frame_complete(sess, DispatchTime.now().uptimeNanoseconds - metricsStart) }}
        }}
        guard let frame = layerRenderer.queryNextFrame() else {{ return }}
        frame.startUpdate()
        frame.endUpdate()
        guard let timing = frame.predictTiming() else {{ return }}
        LayerRenderer.Clock().wait(until: timing.optimalInputTime)
        frame.startSubmission()
        guard let drawable = frame.queryDrawable() else {{ frame.endSubmission(); return }}

        // Session not ready yet: present an empty drawable (black on device — the
        // compositor drops color written without depth) to keep the frame loop and
        // compositor alive while the background build finishes. No scene, no poses.
        guard let sess else {{
            let warmup = commandQueue.makeCommandBuffer()!
            drawable.encodePresent(commandBuffer: warmup)
            warmup.commit()
            frame.endSubmission()
            return
        }}

        // Declare our depth convention to the compositor: reverse-Z, with the far
        // plane in .x and the near plane in .y (metres), matching the Rust render
        // path's FAR/NEAR and its reverse-Z drawable depth. CompositorServices only
        // supports reverse-Z depth and uses these planes to linearise the depth
        // buffer for pose reprojection; leaving the default mis-reads every depth
        // sample, which shows on device as "echo" ghosting and a swimming scene.
        drawable.depthRange = simd_float2(5000.0, 0.05)

        let presentationDuration = LayerRenderer.Clock.Instant.epoch.duration(to: timing.presentationTime)
        let presentationTime = Double(presentationDuration.components.seconds)
            + Double(presentationDuration.components.attoseconds) * 1e-18
        let anchor = worldTracking.queryDeviceAnchor(atTimestamp: presentationTime)
        if let anchor {{ drawable.deviceAnchor = anchor }}
        let originFromDevice = anchor?.originFromAnchorTransform ?? matrix_identity_float4x4

        var input = IdtxFrameInput()
        let hp = originFromDevice.columns.3
        input.head_pos = (hp.x, hp.y, hp.z)
        let hq = simd_quatf(originFromDevice)
        input.head_orientation = (hq.imag.x, hq.imag.y, hq.imag.z, hq.real)

        let viewCount = min(drawable.views.count, 2)
        input.view_count = UInt32(viewCount)
        input.width = UInt32(drawable.colorTextures[0].width)
        input.height = UInt32(drawable.colorTextures[0].height)
        input.color_pixel_format = UInt32(drawable.colorTextures[0].pixelFormat.rawValue)
        input.dt_seconds = Float(lastPresentationTime.map {{ max(0, presentationTime - $0) }} ?? 0)
        lastPresentationTime = presentationTime
        input.timestamp = presentationTime

        for i in 0..<viewCount {{
            let view = drawable.views[i]
            let worldFromView = originFromDevice * view.transform
            let p = worldFromView.columns.3
            let q = simd_quatf(worldFromView)
            // CompositorServices tangents are positive magnitudes (left, right,
            // up, down); the off-axis projection wants left/down negative.
            let t = view.tangents
            let tex = drawable.colorTextures[i]
            let depthTex = i < drawable.depthTextures.count ? drawable.depthTextures[i] : nil
            let depthPtr = depthTex.map {{ UnsafeMutableRawPointer(Unmanaged.passUnretained($0).toOpaque()) }}
            let eye = IdtxEyeInput(
                color_texture: UnsafeMutableRawPointer(Unmanaged.passUnretained(tex).toOpaque()),
                depth_texture: depthPtr,
                pos: (p.x, p.y, p.z),
                orientation: (q.imag.x, q.imag.y, q.imag.z, q.real),
                tangents: (-t[0], t[1], t[2], -t[3]))
            if i == 0 {{ input.eyes.0 = eye }} else {{ input.eyes.1 = eye }}
        }}

        // Feed this frame's hand joints (when supported) BEFORE rendering, so the
        // gesture recogniser + behaviors see them this frame — the Swift side of
        // the hand-input contract (`idtx_visionos_session_set_hand`).
        if Self.handTrackingEnabled && HandTrackingProvider.isSupported {{
            let hands = handTracking.latestAnchors
            pushHand(hands.leftHand, side: 0, session: sess)
            pushHand(hands.rightHand, side: 1, session: sess)
        }}

        _ = withUnsafePointer(to: &input) {{ idtx_visionos_render_frame(sess, $0) }}

        let commandBuffer = commandQueue.makeCommandBuffer()!
        drawable.encodePresent(commandBuffer: commandBuffer)
        commandBuffer.commit()
        frame.endSubmission()
    }}

    // OpenXR XR_HAND_JOINT_*_EXT order (26) → ARKit HandSkeleton joints. ARKit has
    // no palm joint, so PALM (0) reuses the wrist; OpenXR's 3 thumb bones map to
    // ARKit's knuckle / intermediateBase / intermediateTip.
    static let jointOrder: [HandSkeleton.JointName] = [
        .wrist,                                                                                     // 0  PALM (approx)
        .wrist,                                                                                     // 1  WRIST
        .thumbKnuckle, .thumbIntermediateBase, .thumbIntermediateTip, .thumbTip,                    // 2-5  THUMB
        .indexFingerMetacarpal, .indexFingerKnuckle, .indexFingerIntermediateBase, .indexFingerIntermediateTip, .indexFingerTip,       // 6-10  INDEX
        .middleFingerMetacarpal, .middleFingerKnuckle, .middleFingerIntermediateBase, .middleFingerIntermediateTip, .middleFingerTip,  // 11-15 MIDDLE
        .ringFingerMetacarpal, .ringFingerKnuckle, .ringFingerIntermediateBase, .ringFingerIntermediateTip, .ringFingerTip,            // 16-20 RING
        .littleFingerMetacarpal, .littleFingerKnuckle, .littleFingerIntermediateBase, .littleFingerIntermediateTip, .littleFingerTip,  // 21-25 LITTLE
    ]

    // Push one ARKit HandAnchor's 26 joints (STAGE space) into Rust, or clear the
    // hand when absent/untracked. Each joint's world transform is
    // originFromAnchorTransform * anchorFromJointTransform. side: 0 left, 1 right.
    func pushHand(_ anchor: HandAnchor?, side: UInt32, session sess: OpaquePointer) {{
        guard let anchor, anchor.isTracked, let skel = anchor.handSkeleton else {{
            idtx_visionos_session_set_hand(sess, side, nil)
            return
        }}
        let origin = anchor.originFromAnchorTransform
        var pose = IdtxHandPose()
        withUnsafeMutablePointer(to: &pose.joints.0) {{ first in
            for i in 0..<26 {{
                let world = origin * skel.joint(Self.jointOrder[i]).anchorFromJointTransform
                let p = world.columns.3
                let q = simd_quatf(world)
                first[i] = IdtxHandJoint(
                    position: (p.x, p.y, p.z),
                    radius: 0.01,
                    orientation: (q.imag.x, q.imag.y, q.imag.z, q.real))
            }}
        }}
        withUnsafePointer(to: &pose) {{ ptr in
            idtx_visionos_session_set_hand(sess, side, ptr)
        }}
    }}
}}

@main
struct IdtxImmersiveApp: App {{
    @Environment(\.openImmersiveSpace) private var openImmersiveSpace
    @Environment(\.dismissWindow) private var dismissWindow
    static let stagePath: String = {{
        // A single-stage bundle ships a flat `{stage_resource}.{stage_ext}`.
        if let p = Bundle.main.path(forResource: "{stage_resource}", ofType: "{stage_ext}") {{
            return p
        }}
        // A multi-level bundle ships NO flat stage — only `levels/<id>/` plus a
        // `run.toml` carrying `[experience].start_level`. Hand the runtime a
        // bundle-rooted nominal path (the flat resource dir IS the bundle root on
        // visionOS); `build_session` resolves the bundle from `run.toml` and boots
        // `start_level` from there (see `resolve_bundle_dir` / `resolve_open_stage`
        // in apps/idtx-runtime). Passing "" instead would dead-end at an empty
        // root-layer open — the multi-level "app does not start" symptom.
        if let res = Bundle.main.resourcePath {{
            return res + "/{stage_resource}.{stage_ext}"
        }}
        return ""
    }}()

    var body: some Scene {{
        WindowGroup(id: "launch") {{
            // Tiny launch surface: open the immersive space, then dismiss this
            // window so it never occludes the rendered scene. (Without this the
            // panel floats in front of the immersive content and reads as "black".)
            Color.clear
                .frame(width: 1, height: 1)
                .onAppear {{
                    idtxDiag("app onAppear, stage=\(Self.stagePath.isEmpty ? "<MISSING: stage.{stage_ext} not bundled>" : Self.stagePath)")
                    Task {{
                        let result = await openImmersiveSpace(id: "IdtxImmersive")
                        idtxDiag("openImmersiveSpace result=\(result)")
                        dismissWindow(id: "launch")
                    }}
                }}
        }}
        .windowResizability(.contentSize)
        ImmersiveSpace(id: "IdtxImmersive") {{
            CompositorLayer(configuration: IdtxLayerConfiguration()) {{ layerRenderer in
                idtxDiag("CompositorLayer closure entered")
                let renderer = IdtxImmersiveRenderer(layerRenderer, stagePath: Self.stagePath)
                renderer.start()
            }}
        }}
        .immersionStyle(selection: .constant(.full), in: .full)
    }}
}}
"#
    )
}

/// Embed the full dylib load-closure: every `*.dylib` from each of `lib_dirs`
/// copied into `frameworks`. Returns the embedded dylib paths. (`libusd_ms.dylib`
/// hard-depends on `@rpath/libtbb.*` and `@rpath/libMaterialX.*`, so a partial
/// copy would fail to `dlopen`.) `fs::copy` follows symlinks, materializing
/// versioned aliases as real files.
///
/// # Errors
/// [`VisionOsAssembleError::Io`] on a copy/read failure.
pub fn embed_dylib_closure(
    frameworks: &Path,
    lib_dirs: &[PathBuf],
) -> Result<Vec<PathBuf>, VisionOsAssembleError> {
    std::fs::create_dir_all(frameworks).map_err(|source| VisionOsAssembleError::Io {
        path: frameworks.to_path_buf(),
        source,
    })?;
    let mut embedded = Vec::new();
    for dir in lib_dirs {
        if !dir.is_dir() {
            continue;
        }
        let entries = std::fs::read_dir(dir).map_err(|source| VisionOsAssembleError::Io {
            path: dir.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| VisionOsAssembleError::Io {
                path: dir.clone(),
                source,
            })?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_dylib_name(&name) {
                continue;
            }
            let dst = frameworks.join(&name);
            std::fs::copy(entry.path(), &dst).map_err(|source| VisionOsAssembleError::Io {
                path: dst.clone(),
                source,
            })?;
            if !embedded.iter().any(|p| p == &dst) {
                embedded.push(dst);
            }
        }
    }
    Ok(embedded)
}

/// List the `*.dylib` files currently in `frameworks` (used to apply rpath
/// fixups + sign when the closure was already staged by the packager).
fn dylibs_in(frameworks: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(frameworks) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "dylib") {
                out.push(path);
            }
        }
    }
    out
}

// ── macOS orchestration ───────────────────────────────────────────────────────

/// Inputs to [`finish_device_app`].
#[derive(Debug, Clone)]
pub struct DeviceAppPlan<'a> {
    /// The `.app` bundle root (payload + Frameworks already staged).
    pub app: &'a Path,
    /// `<app>/Frameworks`.
    pub frameworks: &'a Path,
    /// The runtime dylib file name (e.g. `libidtx_runtime.dylib`).
    pub runtime_dylib_name: &'a str,
    /// Dirs to gather the dylib closure from; empty if already staged in
    /// `frameworks` (the Conductor path).
    pub lib_dirs: &'a [PathBuf],
    /// Bundled stage resource name (`Bundle.main.path(forResource:)`).
    pub stage_resource: &'a str,
    /// Bundled stage extension (`usda` for a dev probe, `usdz` for a cooked stage).
    pub stage_ext: &'a str,
    /// The visionOS SDK name (`xros`).
    pub sdk: &'a str,
    /// The clang target (`arm64-apple-xros<dep>`, no `-simulator`).
    pub clang_target: &'a str,
    /// The output executable path (the bundle's `CFBundleExecutable`).
    pub exe: &'a Path,
    /// Signing identity (`Apple Development: NAME (TEAMID)`).
    pub identity: &'a str,
    /// Development provisioning profile embedded as `embedded.mobileprovision`.
    pub provisioning_profile: &'a Path,
    /// Signing team id.
    pub team_id: &'a str,
    /// The bundle id (must match the profile's `application-identifier`).
    pub bundle_id: &'a str,
    /// Whether ARKit hand tracking is enabled for this build. MUST equal the
    /// `hand_tracking` passed to the packager's [`visionos_info_plist_with`]: it
    /// gates both the generated Swift `HandTrackingProvider` and the
    /// `NSHandsTrackingUsageDescription` Info.plist key, which must agree or the
    /// app aborts on launch via the ARKit hand-tracking XPC service.
    pub hand_tracking: bool,
}

/// Finish a staged `.app` into a launchable, device-signed bundle: embed the
/// dylib closure (if `lib_dirs` is set), fix install-name/rpaths, compile the
/// Swift immersive shell, embed the provisioning profile, write the development
/// entitlements, and sign (inner dylibs first, then the bundle — no hardened
/// runtime, no notarization).
///
/// # Errors
/// [`VisionOsAssembleError`] on any step; [`VisionOsAssembleError::NotMacOs`] off
/// macOS.
#[cfg(target_os = "macos")]
pub fn finish_device_app(plan: &DeviceAppPlan<'_>) -> Result<(), VisionOsAssembleError> {
    use crate::apple_tools::{
        add_rpath, change_install_name, xcrun_find_tool, xcrun_show_sdk_path,
    };

    // 1. Embed the dylib closure (xtask path); the Conductor path staged it already.
    if !plan.lib_dirs.is_empty() {
        embed_dylib_closure(plan.frameworks, plan.lib_dirs)?;
    }

    // 2. install-name / rpath fixups on every dylib in Frameworks/.
    let runtime = plan.frameworks.join(plan.runtime_dylib_name);
    if runtime.is_file() {
        change_install_name(&runtime, &format!("@rpath/{}", plan.runtime_dylib_name))?;
    }
    for dylib in dylibs_in(plan.frameworks) {
        // Tolerant: @loader_path may already be present.
        add_rpath(&dylib, "@loader_path").ok();
    }

    // 3. Compile the Swift immersive shell against the runtime dylib.
    let sdk_path = xcrun_show_sdk_path(plan.sdk)?;
    let swiftc = xcrun_find_tool(plan.sdk, "swiftc")?;
    let src = plan.app.join(".idtx_main.swift");
    let header = plan.app.join(".idtx_runtime.h");
    write_file(
        &src,
        &swift_shell_source(plan.stage_resource, plan.stage_ext, plan.hand_tracking),
    )?;
    write_file(&header, &c_header())?;
    let args = swiftc_args(
        &sdk_path,
        plan.clang_target,
        &src,
        plan.exe,
        plan.frameworks,
        &header,
    );
    let status = std::process::Command::new(&swiftc)
        .args(&args)
        .status()
        .map_err(|e| VisionOsAssembleError::Swiftc(format!("spawning swiftc: {e}")))?;
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&header);
    if !status.success() {
        return Err(VisionOsAssembleError::Swiftc(format!(
            "{status}. (Is the staged runtime built WITH the `visionos-immersive` feature? \
             The device shell links idtx_visionos_session_create.)"
        )));
    }

    // 4. Embed the development provisioning profile (sealed by the signature).
    let embedded = plan.app.join("embedded.mobileprovision");
    std::fs::copy(plan.provisioning_profile, &embedded).map_err(|source| {
        VisionOsAssembleError::Io {
            path: embedded.clone(),
            source,
        }
    })?;

    // 5. Write the dev entitlements beside the bundle (sealed by the signature
    //    via --entitlements; removed after signing).
    let ents_path = plan
        .app
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".idtx_dev.entitlements");
    write_file(
        &ents_path,
        &visionos_entitlements_plist(plan.team_id, plan.bundle_id),
    )?;

    // 6. Sign for the device (inner dylibs first, then the bundle — handled by the
    //    development signing sequence in SigningPlan).
    let cfg = SigningConfig::VisionOs {
        identity: plan.identity.to_owned(),
        entitlements: Some(ents_path.clone()),
        notary_profile: None,
        provisioning_profile: Some(plan.provisioning_profile.to_path_buf()),
        development: true,
    };
    let result = crate::signing::SigningPlan::new(&cfg, plan.app).execute();
    let _ = std::fs::remove_file(&ents_path);
    result?;
    Ok(())
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn finish_device_app(_plan: &DeviceAppPlan<'_>) -> Result<(), VisionOsAssembleError> {
    Err(VisionOsAssembleError::NotMacOs)
}

#[cfg(target_os = "macos")]
fn write_file(path: &Path, contents: &str) -> Result<(), VisionOsAssembleError> {
    std::fs::write(path, contents).map_err(|source| VisionOsAssembleError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dylib_name_predicate() {
        assert!(is_dylib_name("libusd_ms.dylib"));
        assert!(is_dylib_name("libtbb.12.dylib"));
        assert!(is_dylib_name("libMaterialX.1.39.5.dylib"));
        assert!(!is_dylib_name("plugInfo.json"));
        assert!(!is_dylib_name("libfoo.so"));
    }

    #[test]
    fn entitlements_carry_free_team_dev_keys() {
        let p = visionos_entitlements_plist("TEAMID1234", "com.sap.apeirora.idtx.vision");
        assert!(p.contains("<key>application-identifier</key><string>TEAMID1234.com.sap.apeirora.idtx.vision</string>"));
        assert!(
            p.contains("<key>com.apple.developer.team-identifier</key><string>TEAMID1234</string>")
        );
        assert!(p.contains("<key>get-task-allow</key><true/>"));
        // No ARKit entitlement — world sensing / hand tracking are Info.plist
        // usage strings, not entitlements (and none is available to a free team).
        assert!(!p.contains("arkit"));
    }

    #[test]
    fn swiftc_args_device_target_has_no_simulator_suffix() {
        let args = swiftc_args(
            "/sdk/xros",
            "arm64-apple-xros2.0",
            Path::new("/src/main.swift"),
            Path::new("/app/Idtx"),
            Path::new("/app/Frameworks"),
            Path::new("/app/.idtx_runtime.h"),
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-target" && w[1] == "arm64-apple-xros2.0"),
            "device target must not carry the -simulator suffix"
        );
        assert!(args.contains(&"-lidtx_runtime".to_owned()));
        assert!(args.contains(&"-parse-as-library".to_owned()));
        assert!(args.contains(&"@executable_path/Frameworks".to_owned()));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-import-objc-header" && w[1] == "/app/.idtx_runtime.h")
        );
    }

    #[test]
    fn swift_shell_source_drives_immersive_render_loop() {
        let s = swift_shell_source("stage", "usdz", true);
        assert!(s.contains("idtx_visionos_session_create"));
        assert!(s.contains("idtx_visionos_render_frame"));
        assert!(s.contains("idtx_visionos_session_destroy"));
        assert!(s.contains("ImmersiveSpace"));
        assert!(s.contains("CompositorLayer"));
        assert!(s.contains("queryDrawable"));
        assert!(s.contains("WorldTrackingProvider"));
        // Hand tracking: the ARKit provider is run (device-only, guarded by
        // `.isSupported`) and each frame's joints are pushed into Rust via the
        // `idtx_visionos_session_set_hand` contract BEFORE the render call.
        assert!(s.contains("HandTrackingProvider"));
        assert!(s.contains("HandTrackingProvider.isSupported"));
        assert!(s.contains("idtx_visionos_session_set_hand"));
        assert!(s.contains("func pushHand"));
        assert!(s.contains("latestAnchors"));
        // The compositor's drawable depth is reverse-Z (far in .x, near in .y),
        // matching the Rust render path's FAR/NEAR and reverse-Z depth resolve.
        assert!(s.contains("drawable.depthRange = simd_float2(5000.0, 0.05)"));
        // The stage resource is parameterized.
        assert!(s.contains("forResource: \"stage\", ofType: \"usdz\""));
        assert!(s.contains("IDTX_IMMERSIVE"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires Xcode visionOS SDK"]
    fn generated_swift_shell_typechecks_for_visionos() {
        let sdk = std::process::Command::new("xcrun").args(["--sdk", "xros", "--show-sdk-path"]).output().unwrap();
        assert!(sdk.status.success(), "xros SDK discovery failed");
        let sdk = String::from_utf8(sdk.stdout).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("Runtime.swift");
        let header = directory.path().join("Runtime.h");
        std::fs::write(&source, swift_shell_source("stage", "usdz", true)).unwrap();
        std::fs::write(&header, c_header()).unwrap();
        let output = std::process::Command::new("xcrun").args(["swiftc", "-sdk", sdk.trim(), "-target", "arm64-apple-xros1.0",
            "-parse-as-library", "-typecheck", "-import-objc-header"]).arg(header).arg(source).output().unwrap();
        assert!(output.status.success(), "Swift typecheck: {}", String::from_utf8_lossy(&output.stderr));
    }

    #[test]
    fn swift_shell_gates_hand_tracking_on_the_flag() {
        // REGRESSION GUARD. The shell's ARKit HandTrackingProvider MUST be gated on
        // the same `enable_hand_tracking` flag that controls the
        // `NSHandsTrackingUsageDescription` Info.plist key. If the shell runs hand
        // tracking without that key, ARKit's hand-tracking XPC service throws an
        // uncaught NSException and the app aborts at launch. If a refactor unhooks
        // the shell from the flag (always-on), this test fails on purpose.
        let on = swift_shell_source("stage", "usdz", true);
        let off = swift_shell_source("stage", "usdz", false);
        assert!(on.contains("static let handTrackingEnabled = true"));
        assert!(off.contains("static let handTrackingEnabled = false"));
        // Both `isSupported` checks must be guarded by the flag.
        assert_eq!(
            on.matches("Self.handTrackingEnabled && HandTrackingProvider.isSupported")
                .count(),
            2,
            "both hand-tracking entry points must be gated on the flag"
        );
    }

    #[test]
    fn swift_shell_builds_session_off_the_render_thread() {
        // REGRESSION GUARD. On a real Vision Pro, building the wgpu session inline
        // on the render thread deadlocks: wgpu-core's `Queue::new` eagerly begins a
        // Metal command buffer at device creation, which cannot complete until the
        // compositor advances, which cannot happen until this thread presents a
        // frame — but it is blocked inside `session_create`. The shell MUST build
        // the session on a background queue and present empty frames until it is
        // ready. If a refactor moves the build back onto the render thread, this
        // test fails on purpose. See the big comment in `renderLoop`.
        let s = swift_shell_source("stage", "usdz", true);
        assert!(
            s.contains("DispatchQueue.global(qos: .userInitiated).async"),
            "session must be built on a background queue, not the render thread"
        );
        assert!(
            s.contains("func currentSession()"),
            "render loop must poll for the background-built session"
        );
        assert!(
            s.contains("DO NOT BUILD THE SESSION HERE"),
            "the guardrail rationale comment must stay with the code"
        );
        // The render thread presents an empty drawable while the session builds, so
        // the compositor keeps advancing (which is what unblocks the build).
        assert!(s.contains("drawable.encodePresent(commandBuffer: warmup)"));
    }

    #[test]
    fn swift_shell_source_logs_crash_breadcrumbs_to_pullable_file() {
        let s = swift_shell_source("stage", "usdz", true);
        // A pullable on-device log file (same path the Rust side writes), so an
        // immediate crash BEFORE the Rust FFI is reached is localizable.
        assert!(s.contains("func idtxDiag"));
        assert!(s.contains("idtx_visionos.log"));
        assert!(s.contains("NSTemporaryDirectory()"));
        // Breadcrumbs across the launch → immersive-space → compositor → render
        // thread → FFI lifecycle.
        assert!(s.contains("app onAppear"));
        assert!(s.contains("openImmersiveSpace result="));
        assert!(s.contains("CompositorLayer closure entered"));
        assert!(s.contains("renderer init"));
        assert!(s.contains("renderLoop entered; launching background session build"));
        // A missing bundled stage is called out explicitly rather than passed as
        // an empty path.
        assert!(s.contains("MISSING: stage.usdz not bundled"));
    }

    #[test]
    fn c_header_matches_repr_c_layout() {
        let h = c_header();
        assert!(h.contains("idtx_visionos_session_create"));
        assert!(h.contains("typedef struct IdtxEyeInput"));
        assert!(h.contains("typedef struct IdtxFrameInput"));
        assert!(h.contains("void *color_texture;"));
        assert!(h.contains("IdtxEyeInput eyes[2];"));
        // The hand-input contract: the 26-joint hand pose struct + the setter the
        // Swift shell calls each frame (layout-matched to the `#[repr(C)]` Rust
        // `IdtxHandJoint`/`IdtxHandPose` in `apps/idtx-runtime/src/lib.rs`).
        assert!(h.contains("typedef struct IdtxHandJoint"));
        assert!(h.contains("IdtxHandJoint joints[26];"));
        assert!(h.contains(
            "void idtx_visionos_session_set_hand(VisionosSession *session, uint32_t side, const IdtxHandPose *pose);"
        ));
    }

    #[test]
    fn embed_dylib_closure_copies_only_dylibs() {
        let tmp = std::env::temp_dir().join("idtx-assemble-closure-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let libdir = tmp.join("lib");
        let fw = tmp.join("App.app").join("Frameworks");
        std::fs::create_dir_all(&libdir).unwrap();
        std::fs::write(libdir.join("libusd_ms.dylib"), b"x").unwrap();
        std::fs::write(libdir.join("libtbb.12.dylib"), b"x").unwrap();
        std::fs::write(libdir.join("plugInfo.json"), b"{}").unwrap();
        let embedded = embed_dylib_closure(&fw, std::slice::from_ref(&libdir)).expect("embed");
        assert_eq!(embedded.len(), 2);
        assert!(fw.join("libusd_ms.dylib").is_file());
        assert!(fw.join("libtbb.12.dylib").is_file());
        assert!(!fw.join("plugInfo.json").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
