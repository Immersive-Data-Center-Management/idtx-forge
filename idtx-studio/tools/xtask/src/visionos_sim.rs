//! `cargo xtask run-visionos-sim` — build the **simulator-slice** `idtx-runtime`
//! and run it inside the booted visionOS Simulator on this Mac, with **no physical
//! Vision Pro required**.
//!
//! It assembles a minimal SwiftUI visionOS probe `.app` whose `onAppear` calls the
//! Rust `idtx_runtime_visionos_main(stage_path)` C-ABI entry (see
//! `apps/idtx-runtime/src/lib.rs`) with a bundled `.usda` path. That entry opens
//! the stage through the winit-free `idtx-usd` bridge, so a successful run proves
//! the cross-compiled `OpenUSD` bridge **loads and works on the visionOS runtime** —
//! the strongest on-Mac validation short of real hardware.
//!
//! # Pipeline
//!
//! 1. Cross-build the simulator slice via [`crate::visionos_runtime::build_slice`]
//!    (`aarch64-apple-visionos-sim`, `xrsimulator` SDK, the `-sim` USD install).
//! 2. Assemble `<out>/IdtxVisionProbe.app` by hand (mirroring the macOS bundler in
//!    `main.rs`): a `swiftc`-compiled executable linked against
//!    `libidtx_runtime.dylib`, `Frameworks/` carrying the **full dylib load-closure**
//!    (every `.dylib` from the three sim lib dirs — `libusd_ms.dylib` hard-depends
//!    on `@rpath/libtbb.12.dylib` and `@rpath/libMaterialX.1.dylib`), the `lib/usd`
//!    plugInfo tree beside the dylibs, and the bundled stage. rpath/`install_name`
//!    fixups + ad-hoc codesign reuse the `main.rs` helpers.
//! 3. Boot the "Apple Vision Pro" Simulator, install, and
//!    `simctl launch --console-pty` — streaming the Rust log that reports the stage
//!    opening (`opened USD stage …`).
//!
//! # Testability (TMDD)
//!
//! The pure logic — swiftc/simctl arg vectors, the bundle layout paths, the
//! Info.plist/Swift source generation, and the dylib-gather predicate — is factored
//! into free functions unit-tested here; only orchestration shells out.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::usd_cross::VisionOsSlice;
use crate::visionos_runtime::{
    SliceBuild, build_slice, clang_target, copy_into, copy_tree_recursive, workspace_root,
    xcrun_find_tool, xcrun_show_sdk_path,
};
use crate::{add_rpath, change_install_name, codesign_adhoc};

/// The probe app's bundle name / executable stem.
pub const PROBE_APP_NAME: &str = "IdtxVisionProbe";
/// The probe app's bundle identifier (what `simctl launch` targets).
pub const PROBE_BUNDLE_ID: &str = "com.sap.apeirora.idtx.visionprobe";
/// The fixed resource name the bundled stage is copied to (`<name>.usda`), so the
/// Swift shell can find it via `Bundle.main.path(forResource:ofType:)`.
pub const PROBE_STAGE_RESOURCE: &str = "probe_stage";
/// Default simulated device to boot.
pub const DEFAULT_SIM_DEVICE: &str = "Apple Vision Pro";
/// Default self-contained stage bundled when `--stage` is not given.
pub const DEFAULT_STAGE: &str = "testfiles/usd/authored_visibility.usda";

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Parsed `run-visionos-sim` options.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[allow(clippy::struct_excessive_bools)] // CLI flag bag; each flag is independent.
pub struct RunSimOptions {
    /// Build the `release` profile (`--release`). Overrides an earlier
    /// `--debug`. Default `false` (debug).
    #[arg(long, overrides_with = "debug")]
    pub release: bool,
    /// Build the `debug` profile (the default); overrides an earlier
    /// `--release` (last profile flag wins).
    #[arg(long, overrides_with = "release")]
    pub debug: bool,
    /// The `.usda`/`.usd(z)` stage to bundle + open (`--stage <path>`). Default
    /// [`DEFAULT_STAGE`].
    #[arg(long, default_value = DEFAULT_STAGE, value_name = "PATH")]
    pub stage: PathBuf,
    /// The simulated device name or UDID to boot (`--device`). Default
    /// [`DEFAULT_SIM_DEVICE`].
    #[arg(long, default_value = DEFAULT_SIM_DEVICE, value_name = "NAME_OR_UDID")]
    pub device: String,
    /// Enable the `metrics` feature so the immersive runtime records per-eye
    /// render timings and dumps `idtx-xr-metrics-<stage>-<ts>.json` into the
    /// simulator app's `tmp/` container. `--with-metrics`.
    #[arg(long = "with-metrics", alias = "with_metrics")]
    pub with_metrics: bool,
    /// Resolve + print the plan, but do not shell out.
    #[arg(long)]
    pub dry_run: bool,
}

impl Default for RunSimOptions {
    fn default() -> Self {
        Self {
            release: false,
            debug: false,
            stage: PathBuf::from(DEFAULT_STAGE),
            device: DEFAULT_SIM_DEVICE.to_owned(),
            with_metrics: false,
            dry_run: false,
        }
    }
}

impl RunSimOptions {
    fn profile(&self) -> &'static str {
        // `debug`/`release` are mutually exclusive via overrides_with (last flag
        // wins); either explicitly set resolves the profile, defaulting to debug.
        if self.release && !self.debug { "release" } else { "debug" }
    }
}

// ---------------------------------------------------------------------------
// Pure helpers — process-free, unit-tested
// ---------------------------------------------------------------------------

/// Bundle layout under `out_root`: `(app, exe, frameworks, info_plist, bundled_stage)`.
#[must_use]
pub fn bundle_layout(out_root: &Path) -> BundleLayout {
    let app = out_root.join(format!("{PROBE_APP_NAME}.app"));
    BundleLayout {
        exe: app.join(PROBE_APP_NAME),
        info_plist: app.join("Info.plist"),
        frameworks: app.join("Frameworks"),
        bundled_stage: app.join(format!("{PROBE_STAGE_RESOURCE}.usda")),
        app,
    }
}

/// Resolved paths inside the probe `.app` (flat iOS/visionOS bundle layout — the
/// executable, `Info.plist`, and `Frameworks/` sit at the bundle root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleLayout {
    pub app: PathBuf,
    pub exe: PathBuf,
    pub info_plist: PathBuf,
    pub frameworks: PathBuf,
    pub bundled_stage: PathBuf,
}

/// Whether `name` is a dynamic library file name we embed in `Frameworks/`
/// (`*.dylib`, including versioned names like `libtbb.12.dylib`).
#[must_use]
pub fn is_dylib_name(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|e| e == "dylib")
}

/// `swiftc` arg vector that compiles the probe executable for the simulator slice,
/// linking `libidtx_runtime.dylib` from `frameworks` and baking the bundle-relative
/// rpath so the loader finds the embedded dylibs.
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
        // `@main` requires library-parse mode; a bare single-file compile is
        // otherwise treated as top-level script code and rejects `@main`.
        "-parse-as-library".to_owned(),
        // Import the Rust immersive FFI C declarations (layout-matched structs +
        // the three entry points), so Swift calls them with the C struct layout.
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

/// `simctl boot <device>` args.
#[must_use]
pub fn simctl_boot_args(device: &str) -> Vec<String> {
    vec!["simctl".to_owned(), "boot".to_owned(), device.to_owned()]
}

/// `simctl install booted <app>` args.
#[must_use]
pub fn simctl_install_args(app: &Path) -> Vec<String> {
    vec![
        "simctl".to_owned(),
        "install".to_owned(),
        "booted".to_owned(),
        app.display().to_string(),
    ]
}

/// `simctl launch --console-pty --terminate-running-process booted <bundle-id>`
/// args — streams the app's stdout/stderr (our tracing log) until it exits.
/// `simctl launch --terminate-running-process booted <bundle-id>` args.
///
/// NOT `--console-pty`: the immersive app runs its `CompositorServices` render loop
/// indefinitely (it never exits), so a console-attached launch would block
/// forever. We launch detached, let it render, then screenshot + dump its log.
#[must_use]
pub fn simctl_launch_args(bundle_id: &str) -> Vec<String> {
    vec![
        "simctl".to_owned(),
        "launch".to_owned(),
        "--terminate-running-process".to_owned(),
        "booted".to_owned(),
        bundle_id.to_owned(),
    ]
}

/// `simctl io booted screenshot <path>` args — captures the rendered immersive
/// view as the on-screen proof the stereo render ran.
#[must_use]
pub fn simctl_screenshot_args(path: &Path) -> Vec<String> {
    vec![
        "simctl".to_owned(),
        "io".to_owned(),
        "booted".to_owned(),
        "screenshot".to_owned(),
        path.display().to_string(),
    ]
}

/// The `Info.plist` for the probe app (visionOS simulator). `deployment` is the
/// visionOS min-OS (e.g. `2.0`).
#[must_use]
pub fn info_plist(deployment: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>{PROBE_APP_NAME}</string>
    <key>CFBundleDisplayName</key><string>IDTX Vision Probe</string>
    <key>CFBundleExecutable</key><string>{PROBE_APP_NAME}</string>
    <key>CFBundleIdentifier</key><string>{PROBE_BUNDLE_ID}</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>CFBundleShortVersionString</key><string>1.0</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>MinimumOSVersion</key><string>{deployment}</string>
    <key>UIDeviceFamily</key><array><integer>7</integer></array>
    <key>DTPlatformName</key><string>xrsimulator</string>
    <key>CFBundleSupportedPlatforms</key><array><string>XRSimulator</string></array>
    <key>NSWorldSensingUsageDescription</key><string>Tracks your head pose to render the immersive scene stereoscopically.</string>
    <key>UIApplicationSceneManifest</key>
    <dict>
        <key>UIApplicationSupportsMultipleScenes</key><true/>
        <key>UIApplicationPreferredDefaultSceneSessionRole</key><string>UIWindowSceneSessionRoleApplication</string>
    </dict>
</dict>
</plist>
"#
    )
}

/// The C bridging header (`-import-objc-header`) declaring the Rust immersive
/// FFI — the `IdtxEyeInput`/`IdtxFrameInput` structs (layout-matched to the
/// `#[repr(C)]` Rust types in `apps/idtx-runtime/src/lib.rs`) and the three
/// `idtx_visionos_*` entry points. Importing the C types guarantees the struct
/// layout matches across the boundary (no hand-mirrored Swift struct).
#[must_use]
pub fn c_header() -> String {
    r#"#pragma once
#include <stdint.h>

typedef struct IdtxEyeInput {
    void *color_texture;
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

typedef struct VisionosSession VisionosSession;

VisionosSession *idtx_visionos_session_create(const char *stage_path, void *mtl_device);
int32_t idtx_visionos_render_frame(VisionosSession *session, const IdtxFrameInput *frame);
void idtx_visionos_session_destroy(VisionosSession *session);
"#
    .to_owned()
}

/// The `SwiftUI` immersive-shell source. Opens an `ImmersiveSpace` whose
/// `CompositorLayer` drives a `CompositorServices` render loop: it creates the Rust
/// immersive session (handing it the Metal device + bundled stage), then each
/// frame queries the drawable, samples the ARKit device anchor, builds an
/// `IdtxFrameInput` with the per-eye drawable textures + poses, calls into Rust
/// to render the stereo frame, and presents the drawable.
#[must_use]
pub fn swift_source() -> String {
    format!(
        r#"import SwiftUI
import CompositorServices
import Metal
import ARKit
import simd
import Foundation

// The Rust FFI (idtx_visionos_session_create / _render_frame / _session_destroy)
// and the IdtxFrameInput / IdtxEyeInput structs come from the imported C header.

/// Immersive compositor configuration: a dedicated color texture per eye (what
/// the Rust drawable importer expects), rgba16Float color + depth.
struct IdtxLayerConfiguration: CompositorLayerConfiguration {{
    func makeConfiguration(capabilities: LayerRenderer.Capabilities,
                           configuration: inout LayerRenderer.Configuration) {{
        configuration.depthFormat = .depth32Float
        configuration.colorFormat = .rgba16Float
        configuration.isFoveationEnabled = false
        configuration.layout = .dedicated
    }}
}}

final class IdtxImmersiveRenderer {{
    let layerRenderer: LayerRenderer
    let device: MTLDevice
    let commandQueue: MTLCommandQueue
    let arSession = ARKitSession()
    let worldTracking = WorldTrackingProvider()
    let stagePath: String
    var session: OpaquePointer?

    init(_ layerRenderer: LayerRenderer, stagePath: String) {{
        self.layerRenderer = layerRenderer
        self.device = MTLCreateSystemDefaultDevice()!
        self.commandQueue = self.device.makeCommandQueue()!
        self.stagePath = stagePath
    }}

    func start() {{
        Task {{
            do {{ try await arSession.run([worldTracking]) }}
            catch {{ NSLog("IDTX_IMMERSIVE ARKit run failed: \(error)") }}
        }}
        let thread = Thread {{ [weak self] in self?.renderLoop() }}
        thread.name = "idtx-render"
        thread.start()
    }}

    func renderLoop() {{
        let devPtr = UnsafeMutableRawPointer(Unmanaged.passUnretained(device).toOpaque())
        stagePath.withCString {{ cstr in
            session = idtx_visionos_session_create(cstr, devPtr)
        }}
        if session == nil {{
            NSLog("IDTX_IMMERSIVE session create FAILED for \(stagePath)")
            return
        }}
        NSLog("IDTX_IMMERSIVE session created, entering render loop")
        while true {{
            switch layerRenderer.state {{
            case .paused:
                layerRenderer.waitUntilRunning()
            case .running:
                autoreleasepool {{ renderFrame() }}
            case .invalidated:
                NSLog("IDTX_IMMERSIVE layer invalidated; tearing down")
                idtx_visionos_session_destroy(session)
                session = nil
                return
            @unknown default:
                return
            }}
        }}
    }}

    func renderFrame() {{
        guard let frame = layerRenderer.queryNextFrame() else {{ return }}
        frame.startUpdate()
        frame.endUpdate()
        guard let timing = frame.predictTiming() else {{ return }}
        LayerRenderer.Clock().wait(until: timing.optimalInputTime)
        frame.startSubmission()
        guard let drawable = frame.queryDrawable() else {{ frame.endSubmission(); return }}

        // Declare our depth convention to the compositor: reverse-Z, with the far
        // plane in .x and the near plane in .y (metres), matching the Rust render
        // path's FAR/NEAR and its reverse-Z drawable depth. Kept in sync with the
        // device shell in idtx-build::visionos_assemble; the Simulator is lenient
        // about depth but parity keeps the two emitters identical.
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
        input.dt_seconds = 1.0 / 90.0
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
            let eye = IdtxEyeInput(
                color_texture: UnsafeMutableRawPointer(Unmanaged.passUnretained(tex).toOpaque()),
                pos: (p.x, p.y, p.z),
                orientation: (q.imag.x, q.imag.y, q.imag.z, q.real),
                tangents: (-t[0], t[1], t[2], -t[3]))
            if i == 0 {{ input.eyes.0 = eye }} else {{ input.eyes.1 = eye }}
        }}

        _ = withUnsafePointer(to: &input) {{ idtx_visionos_render_frame(session, $0) }}

        let commandBuffer = commandQueue.makeCommandBuffer()!
        drawable.encodePresent(commandBuffer: commandBuffer)
        commandBuffer.commit()
        frame.endSubmission()
    }}
}}

@main
struct IdtxImmersiveApp: App {{
    @Environment(\.openImmersiveSpace) private var openImmersiveSpace
    static let stagePath = Bundle.main.path(forResource: "{PROBE_STAGE_RESOURCE}", ofType: "usda") ?? ""

    var body: some Scene {{
        WindowGroup {{
            Text("IDTX immersive runtime")
                .padding()
                .onAppear {{
                    NSLog("IDTX_IMMERSIVE launching, stage=\(Self.stagePath)")
                    Task {{ _ = await openImmersiveSpace(id: "IdtxImmersive") }}
                }}
        }}
        ImmersiveSpace(id: "IdtxImmersive") {{
            CompositorLayer(configuration: IdtxLayerConfiguration()) {{ layerRenderer in
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

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// `cargo xtask run-visionos-sim [--release] [--stage <usda>] [--device <name|udid>]
/// [--dry-run]`. macOS only (needs `xcrun`/`simctl`/`swiftc` + the visionOS
/// Simulator); `--dry-run` prints the plan on any host.
pub fn run_visionos_sim_cmd(opts: &RunSimOptions) -> Result<()> {
    if !cfg!(target_os = "macos") && !opts.dry_run {
        bail!(
            "run-visionos-sim must run on macOS (needs xcrun + the visionOS Simulator). \
             Use --dry-run to inspect the plan on another host."
        );
    }

    let root = workspace_root();
    let out_root = root
        .join("target")
        .join(opts.profile())
        .join("visionos-sim");
    let layout = bundle_layout(&out_root);
    let stage_abs = if opts.stage.is_absolute() {
        opts.stage.clone()
    } else {
        root.join(&opts.stage)
    };

    println!("run-visionos-sim plan:");
    println!("  Slice:    aarch64-apple-visionos-sim (xrsimulator)");
    println!("  Profile:  {}", opts.profile());
    println!("  Stage:    {}", stage_abs.display());
    println!("  Device:   {}", opts.device);
    println!("  App:      {}", layout.app.display());
    if opts.with_metrics {
        println!("  Metrics:  on (feature `metrics`)");
    }

    if opts.dry_run {
        println!("run-visionos-sim: --dry-run, stopping before any process is spawned.");
        return Ok(());
    }

    if !stage_abs.is_file() {
        bail!("stage to bundle not found: {}", stage_abs.display());
    }

    // 1) Cross-build the simulator-slice runtime WITH the immersive renderer
    //    (CompositorServices + Metal + ARKit + Pulley behaviors).
    let mut features = vec!["visionos-immersive", "gesture-recognition"];
    if opts.with_metrics {
        features.push("metrics");
    }
    let built = build_slice(
        VisionOsSlice::Simulator,
        opts.release,
        None,
        &features,
    )?;

    // 2) Assemble the immersive `.app`.
    assemble_app(&layout, &built, &stage_abs)?;

    // 3) Boot the Simulator, install, launch, render, screenshot.
    let screenshot = out_root.join("immersive.png");
    run_in_simulator(&opts.device, &layout.app, &screenshot)?;

    println!(
        "\n\u{2705}  Ran {PROBE_APP_NAME} immersive shell in the visionOS Simulator.\n\
         Look for the `IDTX_IMMERSIVE session created, entering render loop` log line and the\n\
         rendered stage in {}.",
        screenshot.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Orchestration (shells out)
// ---------------------------------------------------------------------------

/// Assemble the probe `.app`: embed the dylib load-closure + plugInfo tree + the
/// bundled stage, compile the Swift shell against the runtime dylib, fix rpaths /
/// install names, and ad-hoc codesign.
fn assemble_app(layout: &BundleLayout, built: &SliceBuild, stage: &Path) -> Result<()> {
    // Clean slate.
    if layout.app.exists() {
        std::fs::remove_dir_all(&layout.app)
            .with_context(|| format!("clearing old bundle {}", layout.app.display()))?;
    }
    std::fs::create_dir_all(&layout.frameworks)
        .with_context(|| format!("creating {}", layout.frameworks.display()))?;

    // 2a. Embed the full dylib load-closure: every `.dylib` from the three sim lib
    //     dirs. libusd_ms.dylib hard-depends on @rpath/libtbb.12.dylib AND
    //     @rpath/libMaterialX.1.dylib, so a partial copy would fail to dlopen.
    let mut embedded: Vec<PathBuf> = Vec::new();
    let lib_dirs = [
        built.prefixes.usd_dir.join("lib"),
        built.prefixes.tbb_prefix.join("lib"),
        built.prefixes.materialx_prefix.join("lib"),
    ];
    for dir in &lib_dirs {
        if !dir.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_dylib_name(&name) {
                continue;
            }
            let dst = layout.frameworks.join(&name);
            // `fs::copy` follows symlinks, materializing versioned aliases
            // (libtbb.12.dylib → …) as real files — enough for the loader.
            std::fs::copy(entry.path(), &dst)
                .with_context(|| format!("embedding {name} into Frameworks/"))?;
            if !embedded.iter().any(|p| p == &dst) {
                embedded.push(dst);
            }
        }
    }

    // 2b. The runtime cdylib (install name → @rpath so the exe resolves it via the
    //     bundle rpath; @loader_path rpath so it finds libusd_ms as a sibling).
    let runtime_dst = layout.frameworks.join("libidtx_runtime.dylib");
    copy_into(&built.produced_dylib, &layout.frameworks)?;
    change_install_name(&runtime_dst, "@rpath/libidtx_runtime.dylib")?;
    add_rpath(&runtime_dst, "@loader_path").ok();

    // 2c. Every embedded USD dylib cross-references siblings via @rpath → add
    //     @loader_path so the sibling Frameworks/ dir satisfies them.
    for dylib in &embedded {
        add_rpath(dylib, "@loader_path").ok();
    }

    // 2d. OpenUSD plugInfo discovery tree beside the dylibs (USD discovers plugins
    //     relative to libusd_ms.dylib's directory). Mirrors the macOS bundler.
    let usd_tree = built.prefixes.usd_dir.join("lib").join("usd");
    if usd_tree.is_dir() {
        copy_tree_recursive(&usd_tree, &layout.frameworks.join("usd"))
            .with_context(|| format!("staging USD plugInfo tree {}", usd_tree.display()))?;
    } else {
        eprintln!(
            "  warning: USD plugInfo tree {} absent; opening a stage will fail",
            usd_tree.display()
        );
    }

    // 2e. The bundled stage.
    std::fs::copy(stage, &layout.bundled_stage)
        .with_context(|| format!("bundling stage {}", stage.display()))?;

    // 2f. Info.plist.
    std::fs::write(&layout.info_plist, info_plist(&built.deployment_target))
        .with_context(|| format!("writing {}", layout.info_plist.display()))?;

    // 2g. Compile the Swift shell against the runtime dylib.
    compile_swift(layout, built)?;

    // 2h. Ad-hoc codesign the dylibs, then the whole app (sim requires a valid
    //     signature). Reuses the macOS bundler helpers.
    for dylib in std::iter::once(&runtime_dst).chain(embedded.iter()) {
        codesign_adhoc(dylib, false)?;
    }
    codesign_adhoc(&layout.app, true)?;

    println!(
        "  assembled {} ({} embedded dylibs)",
        layout.app.display(),
        embedded.len() + 1
    );
    Ok(())
}

/// Write the Swift source to a scratch file and compile it into the bundle exe.
fn compile_swift(layout: &BundleLayout, built: &SliceBuild) -> Result<()> {
    let sdk = VisionOsSlice::Simulator.sdk(); // "xrsimulator"
    let sdk_path = xcrun_show_sdk_path(sdk)?;
    let swiftc = xcrun_find_tool(sdk, "swiftc")?;
    let target = clang_target(VisionOsSlice::Simulator, &built.deployment_target);

    let src = layout.app.join(".probe_main.swift");
    std::fs::write(&src, swift_source())
        .with_context(|| format!("writing {}", src.display()))?;

    // The C bridging header with the Rust immersive FFI declarations.
    let header = layout.app.join(".idtx_runtime.h");
    std::fs::write(&header, c_header())
        .with_context(|| format!("writing {}", header.display()))?;

    let args = swiftc_args(&sdk_path, &target, &src, &layout.exe, &layout.frameworks, &header);
    let mut cmd = Command::new(&swiftc);
    cmd.args(&args);
    println!("  $ {cmd:?}");
    let status = cmd.status().context("spawning swiftc for the immersive app")?;
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&header);
    if !status.success() {
        bail!("swiftc failed to compile the visionOS immersive app ({status})");
    }
    Ok(())
}

/// Boot the Simulator device (tolerating "already booted"), reveal the Simulator
/// UI, install the app, launch it detached, let the immersive render loop run,
/// then capture a screenshot + the app's recent log as the render proof.
fn run_in_simulator(device: &str, app: &Path, screenshot: &Path) -> Result<()> {
    boot_device(device)?;
    // Bring the Simulator window up (best-effort; the headless install/launch does
    // not require it, but the on-screen immersive render is the point).
    Command::new("open")
        .args(["-a", "Simulator"])
        .status()
        .ok();
    run_xcrun(&simctl_install_args(app), "simctl install")?;
    // Launch detached: the immersive app never exits (it runs the render loop),
    // so we do NOT attach a console. Give it time to open the immersive space and
    // render several frames, then screenshot + dump the log.
    run_xcrun(&simctl_launch_args(PROBE_BUNDLE_ID), "simctl launch")?;
    println!("  launched; letting the immersive render loop run…");
    std::thread::sleep(std::time::Duration::from_secs(12));

    if let Some(parent) = screenshot.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match Command::new("xcrun")
        .args(simctl_screenshot_args(screenshot))
        .status()
    {
        Ok(s) if s.success() => println!("  screenshot -> {}", screenshot.display()),
        Ok(s) => eprintln!("  screenshot failed ({s})"),
        Err(e) => eprintln!("  screenshot could not spawn: {e}"),
    }

    // Dump the app's recent log so the IDTX_IMMERSIVE lines (session create,
    // render loop, any error) are visible in the terminal.
    println!("  --- recent IDTX_IMMERSIVE log ---");
    Command::new("xcrun")
        .args([
            "simctl",
            "spawn",
            "booted",
            "log",
            "show",
            "--last",
            "20s",
            "--predicate",
            "eventMessage CONTAINS \"IDTX_IMMERSIVE\"",
        ])
        .status()
        .ok();
    Ok(())
}

/// `xcrun simctl boot <device>`, treating an already-booted device as success.
fn boot_device(device: &str) -> Result<()> {
    let out = Command::new("xcrun")
        .args(simctl_boot_args(device))
        .output()
        .context("spawning `xcrun simctl boot`")?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Idempotent: a device already booted is exactly what we want.
    if stderr.contains("Booted") || stderr.contains("current state: Booted") {
        return Ok(());
    }
    bail!(
        "`xcrun simctl boot {device}` failed ({}): {}\n\
         List devices with `xcrun simctl list devices available` and pass --device <name|UDID>.",
        out.status,
        stderr.trim()
    );
}

/// Run `xcrun <args…>` inheriting stdio (so `--console-pty` streams to the user),
/// erroring on non-zero exit.
fn run_xcrun(args: &[String], label: &str) -> Result<()> {
    let mut cmd = Command::new("xcrun");
    cmd.args(args);
    println!("  $ {cmd:?}");
    let status = cmd.status().with_context(|| format!("spawning `{label}`"))?;
    if !status.success() {
        bail!("`{label}` failed with {status}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests — pure logic only
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Parse `RunSimOptions` from a bare argument vector via a minimal clap wrapper.
    fn parse_opts(args: &[&str]) -> Result<RunSimOptions, clap::Error> {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            opts: RunSimOptions,
        }
        TestCli::try_parse_from(std::iter::once("run-visionos-sim").chain(args.iter().copied()))
            .map(|c| c.opts)
    }

    #[test]
    fn options_defaults_and_parse() {
        let o = RunSimOptions::default();
        assert!(!o.release && !o.dry_run);
        assert_eq!(o.device, DEFAULT_SIM_DEVICE);
        assert_eq!(o.stage, PathBuf::from(DEFAULT_STAGE));
        assert_eq!(o.profile(), "debug");

        let o = parse_opts(&["--release", "--stage", "foo.usda", "--device", "UDID-123"]).unwrap();
        assert!(o.release);
        assert_eq!(o.profile(), "release");
        assert_eq!(o.stage, PathBuf::from("foo.usda"));
        assert_eq!(o.device, "UDID-123");
        assert!(parse_opts(&["--bogus"]).is_err());
    }

    #[test]
    fn bundle_layout_is_flat_visionos_app() {
        let l = bundle_layout(Path::new("/out"));
        assert!(l.app.ends_with("IdtxVisionProbe.app"));
        // Flat layout: exe + Info.plist + Frameworks at the bundle root.
        assert_eq!(l.exe, l.app.join("IdtxVisionProbe"));
        assert_eq!(l.info_plist, l.app.join("Info.plist"));
        assert_eq!(l.frameworks, l.app.join("Frameworks"));
        assert_eq!(l.bundled_stage, l.app.join("probe_stage.usda"));
    }

    #[test]
    fn dylib_name_predicate() {
        assert!(is_dylib_name("libusd_ms.dylib"));
        assert!(is_dylib_name("libtbb.12.dylib"));
        assert!(is_dylib_name("libMaterialX.1.39.5.dylib"));
        assert!(!is_dylib_name("plugInfo.json"));
        assert!(!is_dylib_name("libfoo.so"));
    }

    #[test]
    fn swiftc_args_link_runtime_and_bake_rpath() {
        let args = swiftc_args(
            "/sdk/xrsim",
            "arm64-apple-xros2.0-simulator",
            Path::new("/src/main.swift"),
            Path::new("/app/IdtxVisionProbe"),
            Path::new("/app/Frameworks"),
            Path::new("/app/.idtx_runtime.h"),
        );
        assert!(args.windows(2).any(|w| w[0] == "-sdk" && w[1] == "/sdk/xrsim"));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-target" && w[1] == "arm64-apple-xros2.0-simulator")
        );
        assert!(args.contains(&"-lidtx_runtime".to_owned()));
        assert!(args.windows(2).any(|w| w[0] == "-L" && w[1] == "/app/Frameworks"));
        // `@main` SwiftUI app needs library-parse mode.
        assert!(args.contains(&"-parse-as-library".to_owned()));
        // The immersive FFI C declarations are imported via a bridging header.
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-import-objc-header" && w[1] == "/app/.idtx_runtime.h"),
            "the Rust FFI header must be imported"
        );
        // The bundle-relative loader path for the embedded dylibs.
        assert!(args.contains(&"@executable_path/Frameworks".to_owned()));
        assert!(args.last().unwrap().ends_with("main.swift"));
    }

    #[test]
    fn simctl_arg_vectors() {
        assert_eq!(
            simctl_boot_args("Apple Vision Pro"),
            vec!["simctl", "boot", "Apple Vision Pro"]
        );
        assert_eq!(
            simctl_install_args(Path::new("/x/App.app")),
            vec!["simctl", "install", "booted", "/x/App.app"]
        );
        let launch = simctl_launch_args(PROBE_BUNDLE_ID);
        assert!(launch.contains(&"launch".to_owned()));
        assert!(
            !launch.contains(&"--console-pty".to_owned()),
            "the immersive app never exits, so launch must be detached"
        );
        assert!(launch.contains(&"booted".to_owned()));
        assert!(launch.contains(&PROBE_BUNDLE_ID.to_owned()));
        // Screenshot capture is the render proof.
        let shot = simctl_screenshot_args(Path::new("/out/immersive.png"));
        assert_eq!(shot[0..4], ["simctl", "io", "booted", "screenshot"]);
        assert!(shot.last().unwrap().ends_with("immersive.png"));
    }

    #[test]
    fn info_plist_declares_visionos_simulator() {
        let p = info_plist("2.0");
        assert!(p.contains("<string>com.sap.apeirora.idtx.visionprobe</string>"));
        assert!(p.contains("<string>IdtxVisionProbe</string>"));
        assert!(p.contains("<string>XRSimulator</string>"));
        assert!(p.contains("<integer>7</integer>")); // visionOS device family
        assert!(p.contains("<string>2.0</string>")); // MinimumOSVersion
        // ARKit world sensing (head pose) usage description for the immersive run.
        assert!(p.contains("NSWorldSensingUsageDescription"));
    }

    #[test]
    fn swift_source_drives_immersive_render_loop() {
        let s = swift_source();
        // The immersive shell creates the Rust session + per-frame render + teardown.
        assert!(s.contains("idtx_visionos_session_create"));
        assert!(s.contains("idtx_visionos_render_frame"));
        assert!(s.contains("idtx_visionos_session_destroy"));
        // CompositorServices immersive space + drawable loop.
        assert!(s.contains("ImmersiveSpace"));
        assert!(s.contains("CompositorLayer"));
        assert!(s.contains("queryDrawable"));
        // ARKit head pose + the bundled stage resource.
        assert!(s.contains("WorldTrackingProvider"));
        // Reverse-Z drawable depth range (far in .x, near in .y), kept in sync
        // with the device shell in idtx-build::visionos_assemble.
        assert!(s.contains("drawable.depthRange = simd_float2(5000.0, 0.05)"));
        assert!(s.contains("forResource: \"probe_stage\""));
        // The log line the user looks for.
        assert!(s.contains("IDTX_IMMERSIVE"));
    }

    #[test]
    fn c_header_matches_repr_c_layout() {
        let h = c_header();
        // The three FFI entry points + the two layout-matched structs.
        assert!(h.contains("idtx_visionos_session_create"));
        assert!(h.contains("idtx_visionos_render_frame"));
        assert!(h.contains("idtx_visionos_session_destroy"));
        assert!(h.contains("typedef struct IdtxEyeInput"));
        assert!(h.contains("typedef struct IdtxFrameInput"));
        // Field order must mirror the Rust #[repr(C)] structs.
        assert!(h.contains("void *color_texture;"));
        assert!(h.contains("IdtxEyeInput eyes[2];"));
    }
}
