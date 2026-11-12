//! End-to-end desktop-player vertical slice: *build → attach → ▶ →
//! WASD/mouse move the pawn*.
//!
//! Compiles the two bundled behaviors (`desktop_player_move` +
//! `desktop_camera_pitch`) to wasm, opens a hand-authored player rig in a real
//! [`StudioSession`], feeds physical input the way the runtime player does, and
//! asserts the composed transforms respond:
//!
//! * pressing `Key/W` walks the pawn along its forward (`-Z`) axis,
//! * mouse-X yaws the pawn (its local Y rotation changes),
//! * a sustained upward mouse push tilts the camera and **clamps pitch to ±40°**.
//!
//! This is the heavy path: it needs the `OpenUSD` SDK (`IDTX_USD_DIR` +
//! `DYLD_LIBRARY_PATH`), a wgpu adapter, and the `wasm32-unknown-unknown`
//! target. It self-skips when the wasm target is absent and is `#[ignore]` in
//! ordinary runs (see the crate's integration-test env note).

use std::path::{Path, PathBuf};
use std::process::Command;

use glam::{EulerRot, Mat4, Vec4};
use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

/// `true` when the wasm guest target is installed (else the test self-skips).
fn has_wasm_target() -> bool {
    Command::new("rustc")
        .args([
            "--print",
            "target-libdir",
            "--target",
            "wasm32-unknown-unknown",
        ])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Workspace root (this crate's manifest dir → ../..).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

/// Stage a bundled behavior for compilation: copy the repo's
/// `templates/behaviors/<name>/<name>.rs` into `scratch/` as `<name>.rs`.
/// The behavior's capability grant is authored on the stage's IdtxBehaviorAPI
/// schema attributes (see the rig usda below), not a sidecar `idtx.toml`.
/// Returns the copied `.rs` path.
fn stage_behavior_source(root: &Path, name: &str, scratch: &Path) -> PathBuf {
    let src_dir = root.join("templates/behaviors").join(name);
    let rs = scratch.join(format!("{name}.rs"));
    std::fs::copy(src_dir.join(format!("{name}.rs")), &rs).expect("copy .rs");
    rs
}

/// Build a bundled behavior to wasm, returning its emitted `.wasm` path.
fn build(root: &Path, name: &str, scratch: &Path, out_dir: &Path) -> PathBuf {
    let rs = stage_behavior_source(root, name, scratch);
    let sdk = root.join("crates/idtx-behavior-sdk");
    let opts = idtx_behavior_build::BuildOptions::new(out_dir, &sdk);
    let art = idtx_behavior_build::build_behavior(&rs, &opts).expect("build behavior");
    assert!(art.wasm_path.is_file(), "{name}.wasm not emitted");
    art.wasm_path
}

/// USD row-major `[[f32;4];4]` → glam column-major `Mat4` (glam col j = USD row
/// j; translation is USD row 3 = glam `w_axis`). Mirrors `gizmo::mat_from_usd`.
fn mat_from_usd(m: [[f32; 4]; 4]) -> Mat4 {
    Mat4::from_cols(
        Vec4::from_array(m[0]),
        Vec4::from_array(m[1]),
        Vec4::from_array(m[2]),
        Vec4::from_array(m[3]),
    )
}

/// Create a surfaceless wgpu device/queue (mirrors `runtime-core::headless_device`).
fn headless_gpu() -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .expect("a wgpu adapter for the headless test");
        adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("desktop-player-e2e"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await
            .expect("wgpu device")
    })
}

/// The input map the runtime player loads (WASD → move axes, mouse → `look`).
const INPUT_MAP: &str = "\
[actions.move_forward]
kind = \"axis\"
bindings = [{ source = \"Key/W\", scale = 1.0 }, { source = \"Key/S\", scale = -1.0 }]
[actions.move_right]
kind = \"axis\"
bindings = [{ source = \"Key/D\", scale = 1.0 }, { source = \"Key/A\", scale = -1.0 }]
[actions.look]
kind = \"axis2\"
bindings = [\"Mouse/Delta\"]
";

#[test]
#[ignore = "requires OpenUSD SDK (IDTX_USD_DIR + DYLD_LIBRARY_PATH), a wgpu adapter, and the wasm32 target"]
fn desktop_player_moves_and_camera_pitch_clamps() {
    if !has_wasm_target() {
        eprintln!("skipping: wasm32-unknown-unknown target not installed");
        return;
    }
    let root = workspace_root();

    // 1. Compile the two behaviors to wasm (with their authored capabilities).
    let out_dir = root.join("target/behaviors-e2e");
    let scratch = out_dir.join("src");
    std::fs::create_dir_all(&scratch).expect("scratch dir");
    let move_wasm = build(&root, "desktop_player_move", &scratch, &out_dir);
    let pitch_wasm = build(&root, "desktop_camera_pitch", &scratch, &out_dir);

    // 2. Author a player rig referencing the compiled modules by absolute path
    //    (`idtx:behavior:module` = the .wasm the hydrator reads; the capability
    //    grant + declared inputs are authored straight onto the IdtxBehaviorAPI
    //    schema attributes — no sidecar manifest). Contains dynamic WASM paths so
    //    written to a temp directory rather than a static fixture.
    let rig_dir = std::env::temp_dir().join(format!("idtx-player-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&rig_dir).expect("rig tmp dir");
    let rig = rig_dir.join("phase3_desktop_player.usda");
    let usda = format!(
        "#usda 1.0\n\
         (\n    defaultPrim = \"World\"\n    metersPerUnit = 1\n    upAxis = \"Y\"\n)\n\n\
         def Xform \"World\"\n{{\n\
         \x20   def Xform \"Player\"\n    {{\n\
         \x20       custom token idtx:playerRole = \"local\"\n\
         \x20       custom string idtx:playerId = \"\"\n\
         \x20       custom asset idtx:behavior:module = @{move_wasm}@\n\
         \x20       custom uniform token[] idtx:behavior:capabilities = [\"input\", \"stage_edit\"]\n\
         \x20       custom uniform token[] idtx:behavior:inputs = [\"move_forward:axis\", \"move_right:axis\", \"look:axis2\"]\n\
         \x20       custom token idtx:behavior:kind = \"runtime\"\n\
         \x20       custom string idtx:behavior:uuid = \"idtx-dpmv-0000-4000-8000-000000000001\"\n\n\
         \x20       def Xform \"Head\"\n        {{\n\
         \x20           double3 xformOp:translate = (0, 1.5, 0)\n\
         \x20           uniform token[] xformOpOrder = [\"xformOp:translate\"]\n\
         \x20           def Camera \"Camera\"\n            {{\n\
         \x20               custom asset idtx:behavior:module = @{pitch_wasm}@\n\
         \x20               custom uniform token[] idtx:behavior:capabilities = [\"input\", \"stage_edit\"]\n\
         \x20               custom uniform token[] idtx:behavior:inputs = [\"look:axis2\"]\n\
         \x20               custom token idtx:behavior:kind = \"runtime\"\n\
         \x20               custom string idtx:behavior:uuid = \"idtx-dcpt-0000-4000-8000-000000000002\"\n\
         \x20           }}\n        }}\n    }}\n}}\n",
        move_wasm = move_wasm.display(),
        pitch_wasm = pitch_wasm.display(),
    );
    std::fs::write(&rig, usda).expect("write rig usda");

    // 3. Open the rig in a real session and arm the behavior host + input map.
    let (device, queue) = headless_gpu();
    let mut session = StudioSession::open_with_gpu(
        &rig,
        device,
        queue,
        [256, 256],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open rig");
    session.set_behavior_limits(idtx_behavior::Limits::runtime());
    session.set_behavior_playing(true);
    session.set_behavior_input_map(
        idtx_behavior::InputMap::parse(INPUT_MAP).expect("parse input map"),
    );
    // Absolute `idtx:behavior:module` paths resolve through USD's own resolver, so the
    // directory-relative fallback readers are never consulted here.
    let attached = session.attach_authored_behaviors(|_| None);
    assert_eq!(attached, 2, "both behaviors must attach");

    // 3b. Select the local player camera the way the runtime player's `init`
    //     does: with `idtx:playerRole="local"` authored on `/World/Player`, its
    //     child `Camera` is the local player camera and must become the render
    //     viewport (never the synthetic orbit camera).
    let cam_path = session
        .local_player_camera_path()
        .expect("local player camera resolves from the idtx:playerRole=local rig");
    assert_eq!(
        cam_path, "/World/Player/Head/Camera",
        "pawn camera is preferred"
    );
    session.use_scene_camera(cam_path);
    session.render_one_frame(false);
    let cam0 = session
        .last_render_camera()
        .expect("a camera was injected into the rendered frame");
    assert_eq!(
        cam0.prim_path, "/World/Player/Head/Camera",
        "runtime rendered through the player camera, not the orbit camera"
    );
    let view_before = cam0.view;

    let dt = 1.0_f32 / 60.0;
    let tick = |s: &mut StudioSession| {
        s.tick().expect("tick");
        s.tick_behaviors(dt);
    };

    // 4. Movement: hold W for ~30 frames → the pawn walks forward (world -Z).
    session.press_input("Key/W");
    for _ in 0..30 {
        tick(&mut session);
    }
    let player = mat_from_usd(
        session
            .get_world_transform("/World/Player")
            .expect("player world xform"),
    );
    let (_, _, pos) = player.to_scale_rotation_translation();
    assert!(
        pos.z < -0.1,
        "pawn did not advance forward (-Z): z = {}",
        pos.z
    );
    assert!(
        pos.x.abs() < 0.05 && pos.y.abs() < 0.05,
        "drifted off-axis: {pos:?}"
    );
    session.release_input("Key/W");

    // 5. Yaw: mouse-X for a few frames turns the pawn about +Y.
    for _ in 0..5 {
        session.add_mouse_input([120.0, 0.0]);
        tick(&mut session);
    }
    let player = mat_from_usd(
        session
            .get_world_transform("/World/Player")
            .expect("player world xform"),
    );
    let (_, yaw_rot, _) = player.to_scale_rotation_translation();
    let yaw = yaw_rot.to_euler(EulerRot::YXZ).0;
    assert!(
        yaw.abs() > 0.01,
        "mouse-X did not yaw the pawn: yaw = {yaw}"
    );

    // 6. Pitch clamp: a sustained upward push must saturate at +40°, never past.
    let max_pitch = 40.0_f32.to_radians();
    for _ in 0..300 {
        session.add_mouse_input([0.0, -200.0]);
        tick(&mut session);
        let cam = mat_from_usd(
            session
                .get_local_transform("/World/Player/Head/Camera")
                .expect("camera local xform"),
        );
        let pitch = cam
            .to_scale_rotation_translation()
            .1
            .to_euler(EulerRot::XYZ)
            .0;
        assert!(
            pitch <= max_pitch + 1e-3,
            "camera pitch exceeded +40°: {} deg",
            pitch.to_degrees()
        );
    }
    let cam = mat_from_usd(
        session
            .get_local_transform("/World/Player/Head/Camera")
            .expect("camera local xform"),
    );
    let pitch = cam
        .to_scale_rotation_translation()
        .1
        .to_euler(EulerRot::XYZ)
        .0;
    assert!(
        (pitch - max_pitch).abs() < 0.02,
        "camera pitch did not settle at +40°: {} deg",
        pitch.to_degrees()
    );

    // 7. The render camera tracks the behavior-driven pose: after the pawn moved,
    //    yawed, and the camera pitched, a freshly rendered frame's camera `view`
    //    differs from the pre-movement frame — the viewport follows the player —
    //    and it is still the player camera (never a fallback to orbit).
    session.render_one_frame(false);
    let cam1 = session
        .last_render_camera()
        .expect("a camera was injected after the movement/pitch ticks");
    assert_eq!(
        cam1.prim_path, "/World/Player/Head/Camera",
        "render camera stayed on the player camera"
    );
    let before = view_before.to_cols_array();
    let after = cam1.view.to_cols_array();
    let max_delta = before
        .iter()
        .zip(after.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        max_delta > 1e-3,
        "render camera view did not track the driving behavior (max elem delta {max_delta})"
    );
}
