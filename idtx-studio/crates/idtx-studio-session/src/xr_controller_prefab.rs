//! XR controller **prefab** authoring for the normal-hydration path (see
//! `docs/xr-controller-hydration-plan.md`).
//!
//! Unlike the legacy bake path (`xr_controller_mesh`), which mounts *invisible*
//! proxies and re-injects a merged mesh at the tracked pose, this module authors
//! a **visible, plain-reference** controller into an in-memory **session layer**
//! so it hydrates and renders through the ordinary
//! `project_viewport → hydrator → scene_extract` path — skeleton, GeomSubset
//! materials, and authored transform intact.
//!
//! Authored shape per hand (`/World/Player/Controllers/{Left,Right}` — the
//! **rig-embedded** layout of the plan's §1; the controllers/pointers are
//! children of the pawn root, not a global `/XR` scope):
//!
//! ```usda
//! def Xform "Left"                     # POSE Xform — the per-frame OpenXR grip
//! {                                    #   pose is authored onto THIS prim (a
//!     matrix4d xformOp:transform = ..  #   session-layer transform write), and
//!     uniform token[] xformOpOrder = ["xformOp:transform"]
//!     def Xform "Model" (              # inner node carries asset normalization
//!         prepend references = @<asset path>@   # absolute, or relative to the
//!                                               #   root layer's directory
//!     )
//!     {
//!         double3 xformOp:scale = (m, m, m)      # asset MPU → world MPU
//!         double3 xformOp:rotateXYZ = (-90, 0, 0) # asset upAxis → world upAxis
//!         uniform token[] xformOpOrder = [ ... ]
//!     }
//! }
//! ```
//!
//! Because the skeleton's rendered world pose is
//! `GetLocalToWorldTransform(Skeleton)` — the *full ancestor chain* — moving the
//! outer pose Xform drives the whole (skinned or rigid) controller. The scoped
//! `project_viewport_roots` reprojection recomputes that on each pose write; the
//! cheap transform-only fast path does **not** refresh skinned meshes, which is
//! why the pose rides a real USD transform rather than a render-side override.
//!
//! ## Normalization (locked decision #4)
//!
//! The controller asset's `upAxis` / `metersPerUnit` are assumed **not** to match
//! the playable world. [`normalization_ops`] emits the correcting xformOps onto
//! the inner `Model` node (never the stage root) so each asset is placed in world
//! space independently: a uniform scale of `asset_mpu / world_mpu`, plus a −90°
//! rotation about X when the asset is Z-up and the world is Y-up. Mirrors
//! `idtx_scene::coordinate` / `pseudo_root_matrix`, generalized to an explicit
//! world target and shared with the shell's `preview_assembly` sublayer path.

/// USD prim paths of the two controller pose Xforms, **rig-embedded** as
/// children of the pawn root (plan §1.1/§1.4). Shared with the legacy bake
/// path's proxy constants (`xr_controller_mesh::{LEFT,RIGHT}_PROXY_PATH`) so
/// both paths address the same stage locations.
///
/// Because they nest under the already-`def`-ined [`PLAYER_ROOT_PATH`], the
/// subtree is defined without the `def Scope "XR"` bootstrap the retired global
/// `/XR/Controllers` layout needed to survive the `IsDefined` traversal
/// predicate.
pub const LEFT_CONTROLLER_PATH: &str = "/World/Player/Controllers/Left";
pub const RIGHT_CONTROLLER_PATH: &str = "/World/Player/Controllers/Right";

/// USD prim paths of the two pointer-ray pose Xforms, driven by the `OpenXR`
/// **aim** pose (distinct from the grip pose that drives the controllers).
/// Rig-embedded under the pawn root alongside the controllers.
pub const LEFT_POINTER_PATH: &str = "/World/Player/Pointers/Left";
pub const RIGHT_POINTER_PATH: &str = "/World/Player/Pointers/Right";

/// USD prim path of the **pawn root**: the player Xform under `/World`
/// that carries the per-level spawn transform and is the locomotion target
/// The persistent player sublayer (§A.3) authors it as
/// `over "World" { def Xform "Player" { … } }`, outside `/World/Levels`, so it
/// survives every level payload swap.
pub const PLAYER_ROOT_PATH: &str = "/World/Player";

/// USD prim path of the pawn's camera (`def Camera` under [`PLAYER_HEAD_PATH`]).
/// A real `UsdGeomCamera` prim — it hydrates a real `CameraComp` with a stable
/// `PrimId` and is the sole camera the preview renders through: there is no
/// bridge-only fallback camera.
///
/// Desktop nests the camera under the eye-height [`PLAYER_HEAD_PATH`] Xform; the
/// XR rig keeps it a direct player child ([`PLAYER_XR_CAMERA_PATH`]) because the
/// HMD supplies the full head pose (height included). Use [`is_pawn_camera_path`]
/// to recognise either shape.
pub const PLAYER_CAMERA_PATH: &str = "/World/Player/Head/Camera";

/// USD prim path of the pawn's **eye-height `Head` Xform** (desktop rig): the
/// parent of [`PLAYER_CAMERA_PATH`], carrying the ~1.5 m standing eye offset.
///
/// It exists so the pitch behavior — which owns only the `Camera` prim and
/// zeroes its translation each frame (the guest has no scene-read capability) —
/// never disturbs the eye height. `Head` is a **sibling** of the `Capsule` under
/// the player root, not its child: locomotion (and later, collision) moves the
/// player root and both `Head`/`Camera` and `Capsule` follow as one rigid pawn,
/// so eye height stays decoupled from the body proxy's transform.
pub const PLAYER_HEAD_PATH: &str = "/World/Player/Head";

/// USD prim path of the **XR** pawn camera — a direct child of the player root
/// (no `Head`). `OpenXR` supplies the full head pose relative to the floor origin,
/// so an authored eye-height offset would double it.
pub const PLAYER_XR_CAMERA_PATH: &str = "/World/Player/Camera";

/// `true` when `path` is a pawn camera in **either** rig shape: the desktop
/// camera nested under `Head` ([`PLAYER_CAMERA_PATH`]) or the XR camera as a
/// direct player child ([`PLAYER_XR_CAMERA_PATH`]).
///
/// The camera-selection preference (`local_player_camera`) uses this so both
/// rigs resolve to their pawn camera even when other local-role cameras share
/// the subtree.
#[must_use]
pub fn is_pawn_camera_path(path: &str) -> bool {
    path == PLAYER_CAMERA_PATH || path == PLAYER_XR_CAMERA_PATH
}

/// USD prim path of the pawn's visual capsule proxy (`def Xform "Capsule"`
/// under [`PLAYER_ROOT_PATH`]). A body proxy only; it carries no physics.
pub const PLAYER_CAPSULE_PATH: &str = "/World/Player/Capsule";

/// Coordinate settings of a USD stage or asset: its up axis token and
/// meters-per-unit. Used to compute the asset→world normalization.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoordinateSettings {
    /// The `upAxis` token as authored (`"Y"`, `"Z"`, …). Empty is treated Y-up.
    pub up_axis_is_y: bool,
    /// The stage/asset `metersPerUnit`.
    pub meters_per_unit: f64,
}

impl CoordinateSettings {
    /// The renderer / `OpenXR` **stage** convention: metres, Y-up. This is the
    /// Phase-0 world target (deliberately independent of any assembly-root MPU).
    pub const WORLD: Self = Self {
        up_axis_is_y: true,
        meters_per_unit: 1.0,
    };

    /// Builds settings from a raw `upAxis` token and `metersPerUnit`. Matches
    /// `UpAxis::from_usd_token`: only `"Y"`/`"y"` is Y-up; anything else (incl.
    /// empty, which USD defaults per-stage) is treated as Z-up here.
    #[must_use]
    pub fn from_usd(up_axis: &str, meters_per_unit: f64) -> Self {
        Self {
            up_axis_is_y: matches!(up_axis, "Y" | "y"),
            meters_per_unit,
        }
    }
}

/// Computes the xformOp lines that normalize an asset authored in `asset`
/// settings into the `world` settings, or `None` when no correction is needed
/// (identity — authoring nothing keeps the referenced asset byte-for-byte).
///
/// The returned lines are authored, in order, onto the inner `Model` node:
/// `xformOp:scale` (uniform `asset_mpu / world_mpu`) then `xformOp:rotateXYZ`
/// (−90° about X, i.e. authored +Z up → world +Y up), followed by the matching
/// `xformOpOrder`. Only the differing components are emitted.
#[must_use]
pub fn normalization_ops(
    asset: CoordinateSettings,
    world: CoordinateSettings,
) -> Option<Vec<String>> {
    // Scale that maps asset units to world units. World MPU of 0 is nonsensical;
    // guard against a divide-by-zero by falling back to the asset MPU (== metres
    // when world is the default 1.0 anyway).
    let scale = if world.meters_per_unit.abs() > f64::EPSILON {
        asset.meters_per_unit / world.meters_per_unit
    } else {
        asset.meters_per_unit
    };
    let needs_scale = (scale - 1.0).abs() > f64::EPSILON;
    let needs_rotate = asset.up_axis_is_y != world.up_axis_is_y;
    if !needs_scale && !needs_rotate {
        return None;
    }

    let mut ops = Vec::new();
    let mut order: Vec<&str> = Vec::new();
    if needs_scale {
        ops.push(format!(
            "double3 xformOp:scale = ({scale}, {scale}, {scale})"
        ));
        order.push("\"xformOp:scale\"");
    }
    if needs_rotate {
        // Only Y↔Z reconciliation is handled here (the only combination USD's
        // upAxis metadata expresses). Asset Z-up → world Y-up is −90° about X;
        // the inverse (asset Y-up into a Z-up world) is +90°.
        let angle = if world.up_axis_is_y { -90 } else { 90 };
        ops.push(format!("double3 xformOp:rotateXYZ = ({angle}, 0, 0)"));
        order.push("\"xformOp:rotateXYZ\"");
    }
    ops.push(format!(
        "uniform token[] xformOpOrder = [{}]",
        order.join(", ")
    ));
    Some(ops)
}

/// A fixed per-asset **mounting offset**: an additional rotation (and optional
/// translation) that seats a controller model in the `OpenXR` **grip** frame.
///
/// The generic [`normalization_ops`] correction only reconciles the asset's
/// `upAxis` / `metersPerUnit` with the world; it does **not** align a
/// device-specific rig frame (e.g. Meta's `questpro_right`, whose origin is the
/// grip root joint in its own rig convention) with the `OpenXR` grip pose. This
/// offset closes that gap. Authored on a `Mount` Xform that wraps the `Model`
/// node, so it composes *outside* normalization:
/// `grip_pose · mount · normalize · asset`.
///
/// Values are in world units: `rotate_xyz` is Euler XYZ degrees, `translate` is
/// metres, `scale` is per-axis (unit `[1,1,1]` = no scaling). Determined
/// empirically in-headset (see the RGB axis gizmo, hand joints, and pointer ray
/// as calibration aids), and supplied per device from `run.toml`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MountOffset {
    /// Euler XYZ rotation in degrees, applied to the normalized model.
    pub rotate_xyz: [f64; 3],
    /// Translation in world metres, applied after the rotation.
    pub translate: [f64; 3],
    /// Per-axis scale, applied innermost (unit `[1,1,1]` authors no scale op).
    pub scale: [f64; 3],
}

/// A resolved controller asset to author: the asset path (absolute, or relative
/// to the **root layer's** directory) and the asset's own coordinate settings
/// for normalization.
///
/// Relative paths resolve because `open_stage_with_session` anchors the
/// generated session layer's identifier to the root layer's directory (see
/// `StageService::open_with_session`), so a session-authored `references`
/// arc resolves against the same base as the root layer would.
#[derive(Debug, Clone)]
pub struct ControllerAsset {
    /// Path to the controller USDZ/USD/USDC to reference — an absolute path /
    /// fully-qualified asset URI, or a path relative to the root layer's dir.
    pub asset_path: String,
    /// The asset's authored coordinate settings, read from its stage metadata.
    pub settings: CoordinateSettings,
    /// Optional per-asset mounting offset that seats the model in the grip
    /// frame. `None` authors the model byte-identically to the un-offset path.
    pub mount_offset: Option<MountOffset>,
}

/// Authors the identity 4×4 transform op used to seed a pose Xform. The
/// per-frame pose write overwrites the matrix value in place.
///
/// `pub` so the shell's player-stage template
/// (`idtx-studio-shell::player_template`) can seed the pawn-root transform when
/// it composes an XR rig by hand (camera + capsule + controllers + pointers)
/// rather than through [`player_pose_block`], keeping the authored bytes
/// identical to the preview/session paths.
#[must_use]
pub fn identity_pose_ops(indent: &str) -> String {
    format!(
        "{indent}matrix4d xformOp:transform = ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1) )\n\
         {indent}uniform token[] xformOpOrder = [\"xformOp:transform\"]\n"
    )
}

/// Length (world metres) of each debug axis bar — 20 cm, per the request.
const DEBUG_AXIS_LENGTH_M: f64 = 0.2;
/// Cross-section (world metres) of each debug axis bar — thin (5 mm).
const DEBUG_AXIS_THICKNESS_M: f64 = 0.005;

/// Whether to author the RGB axis-gizmo debug bars into each controller pose
/// block. Flip to `false` to drop the gizmo without touching call sites.
const DEBUG_AXIS_GIZMO: bool = true;

/// Authors three thin `Cube` bars rooted at the controller **pose** origin —
/// red +X, green +Y, blue +Z — each [`DEBUG_AXIS_LENGTH_M`] long and
/// [`DEBUG_AXIS_THICKNESS_M`] square in cross-section.
///
/// The bars are emitted as siblings of the inner `Model` node, directly under
/// the pose Xform, and are deliberately **not** run through
/// [`normalization_ops`]. They therefore visualize the raw grip-pose frame (the
/// world metre / Y-up space the `OpenXR` pose is written in), so it is obvious
/// which way "up" points relative to the referenced controller mesh, which the
/// `Model` node re-orients into that same frame.
///
/// Each bar is a unit `Cube` (`size = 1`, spanning ±0.5) scaled thin-and-long
/// then translated by half its length so it grows from the origin along its
/// positive axis. `xformOpOrder` lists translate before scale, so the scale is
/// applied in the cube's local space first and the translate then offsets the
/// scaled bar — anchoring one end at the pose origin.
fn axis_gizmo_block(indent: &str) -> String {
    if !DEBUG_AXIS_GIZMO {
        return String::new();
    }
    let l = DEBUG_AXIS_LENGTH_M;
    let t = DEBUG_AXIS_THICKNESS_M;
    let half = l / 2.0;
    // (prim name, displayColor, translate, scale)
    let axes = [
        (
            "AxisX",
            "(1, 0, 0)",
            format!("({half}, 0, 0)"),
            format!("({l}, {t}, {t})"),
        ),
        (
            "AxisY",
            "(0, 1, 0)",
            format!("(0, {half}, 0)"),
            format!("({t}, {l}, {t})"),
        ),
        (
            "AxisZ",
            "(0, 0, 1)",
            format!("(0, 0, {half})"),
            format!("({t}, {t}, {l})"),
        ),
    ];
    let mut s = String::new();
    for (name, color, translate, scale) in axes {
        s.push_str(&format!(
            "{indent}def Cube \"{name}\"\n{indent}{{\n\
             {indent}    double size = 1\n\
             {indent}    color3f[] primvars:displayColor = [{color}]\n\
             {indent}    bool idtx:castsShadows = 0\n\
             {indent}    double3 xformOp:translate = {translate}\n\
             {indent}    double3 xformOp:scale = {scale}\n\
             {indent}    uniform token[] xformOpOrder = [\"xformOp:translate\", \"xformOp:scale\"]\n\
             {indent}}}\n"
        ));
    }
    s
}

/// Length (world metres) of the pointer ray drawn from the aim pose — 1 m.
const POINTER_RAY_LENGTH_M: f64 = 1.0;
/// Cross-section (world metres) of the pointer ray — thin (4 mm).
const POINTER_RAY_THICKNESS_M: f64 = 0.004;

/// Whether to author the pointer-ray geometry into each pointer pose block.
/// Flip to `false` to drop the ray without touching call sites.
const DEBUG_AIM_POINTER: bool = true;

/// Authors a thin `Cube` ray rooted at the aim-pose origin, extending along
/// **−Z** — the `OpenXR` aim "forward" (pointing) direction — for
/// [`POINTER_RAY_LENGTH_M`], [`POINTER_RAY_THICKNESS_M`] square in cross-section.
///
/// Colored magenta to echo the runtime's aim ray. Like [`axis_gizmo_block`] the
/// ray is authored raw (no normalization) so it visualizes the aim frame the
/// `OpenXR` pose is written in. The unit `Cube` is scaled thin-in-XY / long-in-Z,
/// then translated −half its length in Z so its near end sits at the origin and
/// the far end reaches −length.
fn pointer_ray_block(indent: &str) -> String {
    if !DEBUG_AIM_POINTER {
        return String::new();
    }
    let l = POINTER_RAY_LENGTH_M;
    let t = POINTER_RAY_THICKNESS_M;
    let neg_half = -l / 2.0;
    format!(
        "{indent}def Cube \"Ray\"\n{indent}{{\n\
         {indent}    double size = 1\n\
         {indent}    color3f[] primvars:displayColor = [(1, 0, 1)]\n\
         {indent}    bool idtx:castsShadows = 0\n\
         {indent}    double3 xformOp:translate = (0, 0, {neg_half})\n\
         {indent}    double3 xformOp:scale = ({t}, {t}, {l})\n\
         {indent}    uniform token[] xformOpOrder = [\"xformOp:translate\", \"xformOp:scale\"]\n\
         {indent}}}\n"
    )
}

/// Authors one `def Xform "<name>"` pointer pose block: a per-frame pose Xform
/// (identity transform seeded here, overwritten by the aim pose each frame)
/// holding the [`pointer_ray_block`] geometry.
///
/// Mirrors [`controller_pose_block`] for the aim pose. Mounted under
/// `/World/Player/Pointers/{Left,Right}` (see [`LEFT_POINTER_PATH`]).
#[must_use]
pub fn pointer_pose_block(name: &str) -> String {
    let mut s = format!("        def Xform \"{name}\"\n        {{\n");
    s.push_str(&identity_pose_ops("            "));
    s.push_str(&pointer_ray_block("            "));
    s.push_str("        }\n");
    s
}

/// Authors the inner referenced `Model` node at `indent`: a
/// `def Xform "Model"` with a `prepend references` arc to the asset and the
/// [`normalization_ops`] correction (asset upAxis/MPU → world).
fn model_block(asset: &ControllerAsset, world: CoordinateSettings, indent: &str) -> String {
    let mut s = format!(
        "{indent}def Xform \"Model\" (\n{indent}    prepend references = @{}@\n{indent})\n{indent}{{\n",
        asset.asset_path
    );
    if let Some(ops) = normalization_ops(asset.settings, world) {
        for line in ops {
            s.push_str(&format!("{indent}    {line}\n"));
        }
    }
    s.push_str(&format!("{indent}}}\n"));
    s
}
///
/// The outer Xform is the **pose** node (identity transform seeded here,
/// overwritten per frame). When `asset` is `Some`, an inner `Model` references
/// the asset and carries the normalization ops from [`normalization_ops`];
/// `None` authors a bare pose Xform (no geometry) so the prim still exists as a
/// per-frame pose-write target.
///
/// A debug RGB axis gizmo ([`axis_gizmo_block`]) is authored under the pose
/// Xform alongside the `Model`, so both the preview-sublayer and session-layer
/// paths visualize the grip-pose frame at the controller origin.
///
/// When `asset.mount_offset` is `Some`, the inner `Model` is wrapped in a
/// `def Xform "Mount"` carrying the offset (translate + rotateXYZ, plus
/// `xformOp:scale` when the scale is non-unit), so the model composes as
/// `grip_pose · mount · normalize · asset` — seating it in the grip frame. `None`
/// authors the `Model` directly under the pose Xform, byte-for-byte as before; a
/// unit-scale offset likewise omits the scale op. The gizmo always rides the pose
/// (grip) frame, never the `Mount`.
///
/// Shared with `idtx-studio-shell::preview_assembly`, which authors the same
/// **visible** pose block into the on-disk XR sublayer, so the sublayer path and
/// the session-layer prefab emit byte-identical controller shapes.
#[must_use]
pub fn controller_pose_block(
    name: &str,
    asset: Option<&ControllerAsset>,
    world: CoordinateSettings,
) -> String {
    let mut s = format!("        def Xform \"{name}\"\n        {{\n");
    s.push_str(&identity_pose_ops("            "));
    if let Some(asset) = asset {
        s.push_str(&controller_model_block(asset, world, "            "));
    }
    s.push_str(&axis_gizmo_block("            "));
    s.push_str("        }\n");
    s
}

/// Authors the inner controller geometry — the (optionally mount-wrapped) `Model`
/// reference node — at `indent`, without the surrounding pose Xform or gizmo.
///
/// This is the shared body of [`controller_pose_block`] (which wraps it in the
/// per-frame grip-pose Xform) and [`controller_model_over_layer`] (which wraps it
/// in an `over` onto an already-authored pose Xform at packaged-runtime startup).
/// Factoring it out keeps the mount → normalize → asset composition identical
/// across the session-layer prefab, the on-disk preview sublayer, and the runtime
/// device-injection over.
///
/// When `asset.mount_offset` is `Some`, the `Model` is wrapped in a
/// `def Xform "Mount"` carrying the offset (translate + rotateXYZ, plus
/// `xformOp:scale` when non-unit) so the model composes as
/// `pose · mount · normalize · asset`. `None` authors the `Model` directly at
/// `indent`.
#[must_use]
pub fn controller_model_block(
    asset: &ControllerAsset,
    world: CoordinateSettings,
    indent: &str,
) -> String {
    let inner = format!("{indent}    ");
    match asset.mount_offset {
        Some(m) => {
            let [tx, ty, tz] = m.translate;
            let [rx, ry, rz] = m.rotate_xyz;
            let [sx, sy, sz] = m.scale;
            // Scale is authored innermost, and only when non-unit — so a
            // translate/rotate-only offset stays byte-identical to before.
            let needs_scale = (sx - 1.0).abs() > f64::EPSILON
                || (sy - 1.0).abs() > f64::EPSILON
                || (sz - 1.0).abs() > f64::EPSILON;
            let mut s = format!("{indent}def Xform \"Mount\"\n{indent}{{\n");
            s.push_str(&format!(
                "{inner}double3 xformOp:translate = ({tx}, {ty}, {tz})\n"
            ));
            s.push_str(&format!(
                "{inner}double3 xformOp:rotateXYZ = ({rx}, {ry}, {rz})\n"
            ));
            if needs_scale {
                s.push_str(&format!(
                    "{inner}double3 xformOp:scale = ({sx}, {sy}, {sz})\n"
                ));
                s.push_str(&format!(
                    "{inner}uniform token[] xformOpOrder = [\"xformOp:translate\", \"xformOp:rotateXYZ\", \"xformOp:scale\"]\n"
                ));
            } else {
                s.push_str(&format!(
                    "{inner}uniform token[] xformOpOrder = [\"xformOp:translate\", \"xformOp:rotateXYZ\"]\n"
                ));
            }
            s.push_str(&model_block(asset, world, &inner));
            s.push_str(&format!("{indent}}}\n"));
            s
        }
        None => model_block(asset, world, indent),
    }
}

/// Authors an **over-only** session/runtime layer that injects the resolved
/// device `Model` onto the already-authored controller pose Xforms of a packaged
/// rig-embedded player (`/World/Player/Controllers/{Left,Right}`).
///
/// This is the packaged-runtime twin of the preview path's generated sublayer
/// ([`idtx-studio-shell::preview_assembly::xr_layer_text`]): the preview defines
/// the whole player rig (pose Xforms *and* Models) in a fresh sublayer, but a
/// packaged stage already carries the rig — only the concrete device is unknown
/// until the headset is on, so the runtime resolves it at startup and authors
/// **just** the `Model` reference nodes as an `over` onto the existing pose
/// Xforms. The pose Xforms keep receiving the per-frame `OpenXR` grip pose; the
/// injected `Model` (and its mount / up-axis-units normalization) rides beneath.
///
/// Composed as an anonymous **session layer** at open
/// ([`StageService::open_with_session`]), so it never mutates the packaged root
/// layer on disk. Asset paths inside each [`ControllerAsset`] must therefore be
/// **absolute** — the anonymous session layer has no directory to anchor a
/// relative reference against. A hand with no resolved asset authors no `over`
/// (its pose Xform stays a bare, geometry-less pose target — the runtime capsule
/// fallback applies).
#[must_use]
pub fn controller_model_over_layer(
    left: Option<&ControllerAsset>,
    right: Option<&ControllerAsset>,
    world: CoordinateSettings,
) -> String {
    if left.is_none() && right.is_none() {
        return String::new();
    }
    let mut s = String::from("over \"World\"\n{\n    over \"Player\"\n    {\n");
    s.push_str("        over \"Controllers\"\n        {\n");
    for (name, asset) in [("Left", left), ("Right", right)] {
        let Some(asset) = asset else {
            continue;
        };
        s.push_str(&format!("            over \"{name}\"\n            {{\n"));
        s.push_str(&controller_model_block(asset, world, "                "));
        s.push_str("            }\n");
    }
    s.push_str("        }\n    }\n}\n");
    s
}

/// Default vertical field of view (radians) baked into the pawn camera when no
/// HMD FOV override is supplied (60°).
///
/// `pub` so the shell's player-stage template can seed a freshly-authored rig's
/// camera with the same default the preview path uses via
/// [`player_over_world_layer`]`(None, …)`.
pub const PAWN_CAMERA_DEFAULT_FOV_Y: f64 = 60.0 * std::f64::consts::PI / 180.0;
/// USD camera vertical aperture (tenths of a scene unit → mm, standard 35mm).
const PAWN_CAMERA_VERTICAL_APERTURE: f64 = 24.0;
/// USD camera horizontal aperture (35mm film back).
const PAWN_CAMERA_HORIZONTAL_APERTURE: f64 = 36.0;

/// Authors the pawn's `def Camera "Camera"` block at `indent`: a real
/// `UsdGeomCamera` so it hydrates a `CameraComp` with a stable `PrimId`.
///
/// The transform is seeded to identity (a per-frame pose-write target for the
/// HMD-sync behavior, exactly like the controller pose Xforms). `focalLength`
/// is derived from `fov_y_radians` for the fixed [`PAWN_CAMERA_VERTICAL_APERTURE`]
/// so the authored camera matches the HMD's vertical FOV; `clippingRange` is
/// `(near, far)`.
#[must_use]
pub fn player_camera_block(fov_y_radians: f64, near: f64, far: f64, indent: &str) -> String {
    // focalLength = (verticalAperture / 2) / tan(fovY / 2)
    let focal_length = (PAWN_CAMERA_VERTICAL_APERTURE / 2.0) / (fov_y_radians / 2.0).tan();
    let mut s = format!("{indent}def Camera \"Camera\"\n{indent}{{\n");
    s.push_str(&identity_pose_ops(&format!("{indent}    ")));
    s.push_str(&format!("{indent}    float focalLength = {focal_length}\n"));
    s.push_str(&format!(
        "{indent}    float horizontalAperture = {PAWN_CAMERA_HORIZONTAL_APERTURE}\n"
    ));
    s.push_str(&format!(
        "{indent}    float verticalAperture = {PAWN_CAMERA_VERTICAL_APERTURE}\n"
    ));
    s.push_str(&format!(
        "{indent}    float2 clippingRange = ({near}, {far})\n"
    ));
    s.push_str(&format!("{indent}    token projection = \"perspective\"\n"));
    s.push_str(&format!("{indent}}}\n"));
    s
}

/// Total height (world metres) of the visual capsule proxy — a ~1.7 m body.
const PAWN_CAPSULE_HEIGHT_M: f64 = 1.7;
/// Radius (world metres) of the capsule proxy.
const PAWN_CAPSULE_RADIUS_M: f64 = 0.25;

/// Standing eye height (world metres) of the desktop pawn — the `Head` Xform's
/// vertical offset above the player origin (feet). ~1.5 m puts the camera at a
/// typical adult eye level.
const PAWN_EYE_HEIGHT_M: f64 = 1.5;

/// Authors the desktop pawn's `def Xform "Head"` block at `indent`: an
/// eye-height Xform ([`PAWN_EYE_HEIGHT_M`] up the +Y axis) holding the
/// [`player_camera_block`] camera.
///
/// The `Head` decouples the standing eye height from the pitch behavior: the
/// behavior owns only the `Camera` prim and rewrites its local transform (a pure
/// rotation, translation zeroed) each frame, so the eye offset must live on a
/// parent it never touches. XR omits `Head` — the HMD drives the camera pose
/// (height included) directly.
#[must_use]
pub fn player_head_block(fov_y_radians: f64, near: f64, far: f64, indent: &str) -> String {
    let mut s = format!(
        "{indent}def Xform \"Head\"\n{indent}{{\n\
         {indent}    double3 xformOp:translate = (0, {PAWN_EYE_HEIGHT_M}, 0)\n\
         {indent}    uniform token[] xformOpOrder = [\"xformOp:translate\"]\n"
    );
    s.push_str(&player_camera_block(
        fov_y_radians,
        near,
        far,
        &format!("{indent}    "),
    ));
    s.push_str(&format!("{indent}}}\n"));
    s
}

/// Authors the pawn's `def Xform "Capsule"` visual proxy at `indent`: a Y-axis
/// [`UsdGeomCapsule`] standing on the ground plane (feet at the pawn origin),
/// ~1.7 m tall. Visual only — no physics.
#[must_use]
pub fn player_capsule_block(indent: &str) -> String {
    // A UsdGeomCapsule's `height` is the cylinder segment only; the two
    // hemispherical caps add `radius` on each end, so total = height + 2·radius.
    let cyl_height = PAWN_CAPSULE_HEIGHT_M - 2.0 * PAWN_CAPSULE_RADIUS_M;
    // Center the body so its lowest point sits at y = 0 (feet on the ground).
    let center_y = PAWN_CAPSULE_HEIGHT_M / 2.0;
    format!(
        "{indent}def Xform \"Capsule\"\n{indent}{{\n\
         {indent}    double3 xformOp:translate = (0, {center_y}, 0)\n\
         {indent}    uniform token[] xformOpOrder = [\"xformOp:translate\"]\n\
         {indent}    def Capsule \"Proxy\"\n{indent}    {{\n\
         {indent}        uniform token axis = \"Y\"\n\
         {indent}        double height = {cyl_height}\n\
         {indent}        double radius = {PAWN_CAPSULE_RADIUS_M}\n\
         {indent}        color3f[] primvars:displayColor = [(0.2, 0.5, 0.9)]\n\
         {indent}    }}\n{indent}}}\n"
    )
}

/// Authors the pawn-root `def Xform "Player"` block at `indent`: the locomotion
/// / spawn transform Xform (identity-seeded — a per-frame pose-write target)
/// holding the eye-height [`player_head_block`] (camera) and the
/// [`player_capsule_block`] body proxy.
///
/// This is the §A.3 pawn root that replaces the bridge-only synthetic preview
/// camera: the camera is a real USD prim under `/World/Player/Head`, so it
/// hydrates and renders through the ordinary path.
#[must_use]
pub fn player_pose_block(fov_y_radians: f64, near: f64, far: f64, indent: &str) -> String {
    let mut s = format!("{indent}def Xform \"Player\"\n{indent}{{\n");
    s.push_str(&identity_pose_ops(&format!("{indent}    ")));
    s.push_str(&player_head_block(
        fov_y_radians,
        near,
        far,
        &format!("{indent}    "),
    ));
    s.push_str(&player_capsule_block(&format!("{indent}    ")));
    s.push_str(&format!("{indent}}}\n"));
    s
}

/// Serializes the **`over "World"`** pawn-root block for the persistent player
/// sublayer (§A.3): `over "World" { def Xform "Player" { camera + capsule } }`.
///
/// `/World` is already `def`-ined by the scene sublayer (it is the default
/// prim), and `Player`/`Camera`/`Capsule` are all authored as concrete `def`s,
/// so — unlike the `over "XR"` block — no root-layer bootstrap scope is needed
/// to survive the `IsDefined` traversal predicate.
///
/// Pass the HMD's vertical FOV (radians) plus clip range; use
/// [`PAWN_CAMERA_DEFAULT_FOV_Y`] when no HMD override is known.
#[must_use]
pub fn player_over_world_layer(fov_y_radians: Option<f64>, near: f64, far: f64) -> String {
    let fov = fov_y_radians.unwrap_or(PAWN_CAMERA_DEFAULT_FOV_Y);
    let player = player_pose_block(fov, near, far, "    ");
    format!("over \"World\"\n{{\n{player}}}\n")
}

/// Authors the XR pawn's `Controllers` + `Pointers` child scopes — one per-hand
/// pose Xform each — as children of the pawn root (`def Xform "Player"`), at
/// `scope_indent`.
///
/// `left`/`right` optionally carry a device `Model` (referenced + normalized via
/// [`controller_pose_block`]); `None` authors a **bare** pose target (no
/// geometry) that a referencing peer or the packaged runtime injects the mesh
/// into at runtime. The pointer scopes always carry the aim-pose ray geometry
/// (compile-gated in [`pointer_pose_block`]).
///
/// Shared verbatim by the preview path ([`player_rig_over_world_layer`]) and the
/// persistent rig authoring (`idtx-studio-shell::player_template`) so both emit
/// the identical rig-embedded controller/pointer shape.
#[must_use]
pub fn xr_controller_pointer_scopes(
    left: Option<&ControllerAsset>,
    right: Option<&ControllerAsset>,
    world: CoordinateSettings,
    scope_indent: &str,
) -> String {
    let mut s = format!("{scope_indent}def Xform \"Controllers\"\n{scope_indent}{{\n");
    s.push_str(&controller_pose_block("Left", left, world));
    s.push_str(&controller_pose_block("Right", right, world));
    s.push_str(&format!("{scope_indent}}}\n"));
    s.push_str(&format!(
        "{scope_indent}def Xform \"Pointers\"\n{scope_indent}{{\n"
    ));
    s.push_str(&pointer_pose_block("Left"));
    s.push_str(&pointer_pose_block("Right"));
    s.push_str(&format!("{scope_indent}}}\n"));
    s
}

/// Authors the **XR** pawn-root `def Xform "Player"` block at `indent`: identity
/// pose + a **direct** `Camera` child (no eye-height `Head` — the HMD supplies
/// the full head pose, so an authored offset would double it) + the capsule body
/// proxy + the [`xr_controller_pointer_scopes`] pose targets.
///
/// Contrast [`player_pose_block`], the desktop shape whose camera nests under an
/// eye-height `Head`. The XR camera lands at [`PLAYER_XR_CAMERA_PATH`].
#[must_use]
pub fn player_pose_block_xr(
    fov_y_radians: f64,
    near: f64,
    far: f64,
    left: Option<&ControllerAsset>,
    right: Option<&ControllerAsset>,
    world: CoordinateSettings,
    indent: &str,
) -> String {
    let child = format!("{indent}    ");
    let mut s = format!("{indent}def Xform \"Player\"\n{indent}{{\n");
    s.push_str(&identity_pose_ops(&child));
    s.push_str(&player_camera_block(fov_y_radians, near, far, &child));
    s.push_str(&player_capsule_block(&child));
    s.push_str(&xr_controller_pointer_scopes(left, right, world, &child));
    s.push_str(&format!("{indent}}}\n"));
    s
}

/// Serializes the **rig-embedded** `over "World"` player block for the XR
/// preview/persistent sublayer: `over "World" { def Xform "Player" { camera +
/// capsule + Controllers/{Left,Right} + Pointers/{Left,Right} } }`.
///
/// Unlike the retired global `/XR/Controllers` layout, the controllers/pointers
/// are children of the already-`def`-ined `/World/Player`, so **no** `def Scope
/// "XR"` root-layer bootstrap is needed to survive the `IsDefined` traversal
/// predicate.
///
/// `left`/`right` optionally reference each hand's device model (preview
/// resolves these from the manifest); `None` authors bare pose targets. Pass the
/// HMD's vertical FOV (radians) plus clip range, or [`None`] for
/// [`PAWN_CAMERA_DEFAULT_FOV_Y`].
#[must_use]
pub fn player_rig_over_world_layer(
    fov_y_radians: Option<f64>,
    near: f64,
    far: f64,
    left: Option<&ControllerAsset>,
    right: Option<&ControllerAsset>,
    world: CoordinateSettings,
) -> String {
    let fov = fov_y_radians.unwrap_or(PAWN_CAMERA_DEFAULT_FOV_Y);
    let player = player_pose_block_xr(fov, near, far, left, right, world, "    ");
    format!("over \"World\"\n{{\n{player}}}\n")
}

/// Serializes the controller prefab **session layer**: a `def Scope "XR"` with
/// a `Controllers` Xform holding one visible pose Xform per resolved hand.
///
/// Authored as concrete `def`s in the session layer (the stage's strongest
/// layer), so the subtree is defined regardless of the root layer — no
/// `over`-vs-`def` traversal-predicate hazard, and no root-layer `def Scope`
/// bootstrap needed (contrast the sublayer path in
/// `idtx-studio-shell::preview_assembly`).
///
/// `world` is the target the assets are normalized into — pass
/// [`CoordinateSettings::WORLD`] for the metre / Y-up render+XR stage.
#[must_use]
pub fn controller_prefab_session_layer(
    left: Option<&ControllerAsset>,
    right: Option<&ControllerAsset>,
    world: CoordinateSettings,
) -> String {
    let mut body = String::new();
    if let Some(asset) = left {
        body.push_str(&controller_pose_block("Left", Some(asset), world));
    }
    if let Some(asset) = right {
        body.push_str(&controller_pose_block("Right", Some(asset), world));
    }
    format!(
        "#usda 1.0\n(\n    doc = \"IDTX XR controller prefab (session layer, generated)\"\n)\n\n\
         def Scope \"XR\"\n{{\n    def Xform \"Controllers\"\n    {{\n{body}    }}\n}}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_settings_need_no_correction() {
        // Asset already metres + Y-up → nothing to author.
        let asset = CoordinateSettings::from_usd("Y", 1.0);
        assert!(normalization_ops(asset, CoordinateSettings::WORLD).is_none());
    }

    #[test]
    fn centimetre_z_up_asset_scales_and_rotates() {
        // A 1 cm-per-unit, Z-up asset into a metre, Y-up world.
        let asset = CoordinateSettings::from_usd("Z", 0.01);
        let ops = normalization_ops(asset, CoordinateSettings::WORLD).expect("correction");
        assert_eq!(ops.len(), 3, "scale + rotate + order");
        assert_eq!(ops[0], "double3 xformOp:scale = (0.01, 0.01, 0.01)");
        assert_eq!(ops[1], "double3 xformOp:rotateXYZ = (-90, 0, 0)");
        assert_eq!(
            ops[2],
            "uniform token[] xformOpOrder = [\"xformOp:scale\", \"xformOp:rotateXYZ\"]"
        );
    }

    #[test]
    fn z_up_but_metres_rotates_only() {
        let asset = CoordinateSettings::from_usd("Z", 1.0);
        let ops = normalization_ops(asset, CoordinateSettings::WORLD).expect("rotate");
        assert_eq!(ops.len(), 2, "rotate + order");
        assert_eq!(ops[0], "double3 xformOp:rotateXYZ = (-90, 0, 0)");
        assert_eq!(
            ops[1],
            "uniform token[] xformOpOrder = [\"xformOp:rotateXYZ\"]"
        );
    }

    #[test]
    fn scale_derives_from_ratio_not_absolute_mpu() {
        // Asset in cm, world in mm: ratio 0.01 / 0.001 = 10.
        let asset = CoordinateSettings::from_usd("Y", 0.01);
        let world = CoordinateSettings {
            up_axis_is_y: true,
            meters_per_unit: 0.001,
        };
        let ops = normalization_ops(asset, world).expect("scale");
        assert_eq!(ops[0], "double3 xformOp:scale = (10, 10, 10)");
    }

    #[test]
    fn session_layer_authors_visible_pose_xform_with_referenced_model() {
        let left = ControllerAsset {
            asset_path: "C:/assets/controller_left.usdz".to_string(),
            settings: CoordinateSettings::from_usd("Z", 0.01),
            mount_offset: None,
        };
        let text = controller_prefab_session_layer(Some(&left), None, CoordinateSettings::WORLD);

        assert!(text.starts_with("#usda 1.0"));
        assert!(text.contains("def Scope \"XR\""));
        assert!(text.contains("def Xform \"Controllers\""));
        assert!(text.contains("def Xform \"Left\""));
        // Pose Xform seeded with an identity transform op for per-frame writes.
        assert!(text.contains("matrix4d xformOp:transform = ( (1, 0, 0, 0)"));
        // Reference + normalization ride the inner Model node.
        assert!(text.contains(
            "def Xform \"Model\" (\n                prepend references = @C:/assets/controller_left.usdz@"
        ));
        assert!(text.contains("double3 xformOp:scale = (0.01, 0.01, 0.01)"));
        assert!(text.contains("double3 xformOp:rotateXYZ = (-90, 0, 0)"));
        // No proxy visibility override — this controller renders.
        assert!(!text.contains("visibility = \"invisible\""));
        // Right hand omitted → only one pose block.
        assert!(!text.contains("def Xform \"Right\""));
    }

    #[test]
    fn both_hands_emit_two_pose_blocks() {
        let mk = |p: &str| ControllerAsset {
            asset_path: p.to_string(),
            settings: CoordinateSettings::WORLD,
            mount_offset: None,
        };
        let text = controller_prefab_session_layer(
            Some(&mk("/a/left.usdz")),
            Some(&mk("/a/right.usdz")),
            CoordinateSettings::WORLD,
        );
        assert!(text.contains("def Xform \"Left\""));
        assert!(text.contains("def Xform \"Right\""));
        assert!(text.contains("@/a/left.usdz@"));
        assert!(text.contains("@/a/right.usdz@"));
    }

    #[test]
    fn pose_block_without_asset_is_a_bare_visible_xform() {
        // No asset → a bare pose Xform (still visible, still a pose-write target)
        // with the identity transform seeded but no referenced Model / geometry.
        let text = controller_pose_block("Left", None, CoordinateSettings::WORLD);
        assert!(text.contains("def Xform \"Left\""));
        assert!(text.contains("matrix4d xformOp:transform = ( (1, 0, 0, 0)"));
        assert!(!text.contains("def Xform \"Model\""), "no geometry: {text}");
        assert!(!text.contains("references"), "no reference: {text}");
        assert!(
            !text.contains("visibility = \"invisible\""),
            "bare pose block must stay visible: {text}"
        );
    }

    #[test]
    fn pose_block_authors_rgb_axis_gizmo_at_origin() {
        // The debug gizmo must add three colored Cube bars, unnormalized, under
        // the pose Xform — visualizing the grip frame at the controller origin.
        let text = controller_pose_block("Left", None, CoordinateSettings::WORLD);
        assert!(text.contains("def Cube \"AxisX\""), "{text}");
        assert!(text.contains("def Cube \"AxisY\""), "{text}");
        assert!(text.contains("def Cube \"AxisZ\""), "{text}");
        // RGB → XYZ mapping.
        assert!(text.contains("color3f[] primvars:displayColor = [(1, 0, 0)]"));
        assert!(text.contains("color3f[] primvars:displayColor = [(0, 1, 0)]"));
        assert!(text.contains("color3f[] primvars:displayColor = [(0, 0, 1)]"));
        // 20 cm bars, offset by half-length so one end sits at the origin.
        assert!(
            text.contains("double3 xformOp:scale = (0.2, 0.005, 0.005)"),
            "{text}"
        );
        assert!(
            text.contains("double3 xformOp:translate = (0.1, 0, 0)"),
            "{text}"
        );
        // The gizmo rides the pose frame directly — it is not a normalized Model.
        assert!(!text.contains("def Xform \"Model\""), "{text}");
    }

    #[test]
    fn pose_block_with_asset_matches_session_layer_shape() {
        // The shared generator must emit the same pose-block shape the full
        // session layer embeds (the shell reuses this exact function).
        let asset = ControllerAsset {
            asset_path: "/a/left.usdz".to_string(),
            settings: CoordinateSettings::from_usd("Z", 0.01),
            mount_offset: None,
        };
        let block = controller_pose_block("Left", Some(&asset), CoordinateSettings::WORLD);
        let layer = controller_prefab_session_layer(Some(&asset), None, CoordinateSettings::WORLD);
        assert!(
            layer.contains(block.trim_end()),
            "session layer must embed the shared pose block verbatim:\n block=\n{block}\n layer=\n{layer}"
        );
    }

    #[test]
    fn pointer_pose_block_authors_magenta_neg_z_ray() {
        // The pointer ray must be a magenta Cube extending along -Z (OpenXR aim
        // forward) from the aim origin, seeded with an identity pose transform.
        let text = pointer_pose_block("Left");
        assert!(text.contains("def Xform \"Left\""), "{text}");
        assert!(
            text.contains("matrix4d xformOp:transform = ( (1, 0, 0, 0)"),
            "{text}"
        );
        assert!(text.contains("def Cube \"Ray\""), "{text}");
        assert!(
            text.contains("color3f[] primvars:displayColor = [(1, 0, 1)]"),
            "{text}"
        );
        // 1 m ray, thin in X/Y and long in Z, translated -half so the near end
        // sits at the origin and the far end reaches -1.
        assert!(
            text.contains("double3 xformOp:scale = (0.004, 0.004, 1)"),
            "{text}"
        );
        assert!(
            text.contains("double3 xformOp:translate = (0, 0, -0.5)"),
            "{text}"
        );
        // No referenced Model / normalization — the ray rides the aim frame raw.
        assert!(!text.contains("def Xform \"Model\""), "{text}");
    }

    #[test]
    fn mount_offset_wraps_model_in_mount_node() {
        // A mount offset must wrap the Model in a Mount Xform carrying the
        // translate + rotate, composed outside the normalization on Model.
        let asset = ControllerAsset {
            asset_path: "/a/questpro.usdz".to_string(),
            settings: CoordinateSettings::from_usd("Z", 1.0),
            mount_offset: Some(MountOffset {
                rotate_xyz: [45.0, 0.0, 0.0],
                translate: [0.0, 0.0, 0.0],
                scale: [1.0, 1.0, 1.0],
            }),
        };
        let text = controller_pose_block("Right", Some(&asset), CoordinateSettings::WORLD);
        assert!(text.contains("def Xform \"Mount\""), "{text}");
        // The mount rotate is authored, and the Mount node precedes the Model.
        let mount_at = text.find("def Xform \"Mount\"").expect("Mount node");
        let model_at = text.find("def Xform \"Model\"").expect("Model node");
        assert!(mount_at < model_at, "Mount must wrap Model: {text}");
        assert!(
            text.contains("double3 xformOp:rotateXYZ = (45, 0, 0)"),
            "{text}"
        );
        assert!(
            text.contains(
                "uniform token[] xformOpOrder = [\"xformOp:translate\", \"xformOp:rotateXYZ\"]"
            ),
            "{text}"
        );
        // Normalization still rides the inner Model (Z-up → -90 about X).
        assert!(
            text.contains("double3 xformOp:rotateXYZ = (-90, 0, 0)"),
            "{text}"
        );
    }

    #[test]
    fn mount_offset_with_non_unit_scale_emits_scale_op() {
        // A non-unit scale must author `xformOp:scale` innermost, with the
        // 3-op xformOpOrder (translate, rotateXYZ, scale).
        let asset = ControllerAsset {
            asset_path: "/a/questpro.usdz".to_string(),
            settings: CoordinateSettings::WORLD,
            mount_offset: Some(MountOffset {
                rotate_xyz: [180.0, 0.0, 0.0],
                translate: [0.0, 0.0, 0.0],
                scale: [2.0, 2.0, 2.0],
            }),
        };
        let text = controller_pose_block("Right", Some(&asset), CoordinateSettings::WORLD);
        assert!(text.contains("double3 xformOp:scale = (2, 2, 2)"), "{text}");
        assert!(
            text.contains(
                "uniform token[] xformOpOrder = [\"xformOp:translate\", \"xformOp:rotateXYZ\", \"xformOp:scale\"]"
            ),
            "{text}"
        );
    }

    #[test]
    fn no_mount_offset_is_byte_identical_to_bare_model() {
        // The None path must author exactly as the pre-mount-offset generator
        // did: a Model directly under the pose Xform, no Mount node.
        let asset = ControllerAsset {
            asset_path: "/a/left.usdz".to_string(),
            settings: CoordinateSettings::from_usd("Z", 0.01),
            mount_offset: None,
        };
        let text = controller_pose_block("Left", Some(&asset), CoordinateSettings::WORLD);
        assert!(!text.contains("def Xform \"Mount\""), "{text}");
        assert!(
            text.contains(
                "def Xform \"Model\" (\n                prepend references = @/a/left.usdz@"
            ),
            "{text}"
        );
        assert!(
            text.contains("double3 xformOp:scale = (0.01, 0.01, 0.01)"),
            "{text}"
        );
    }

    #[test]
    fn player_block_authors_camera_and_capsule_under_player_root() {
        let text = player_pose_block(PAWN_CAMERA_DEFAULT_FOV_Y, 0.05, 10_000.0, "    ");
        assert!(text.contains("def Xform \"Player\""), "{text}");
        // The camera is nested under an eye-height `Head` Xform (desktop rig).
        assert!(text.contains("def Xform \"Head\""), "{text}");
        assert!(text.contains("def Camera \"Camera\""), "{text}");
        assert!(text.contains("def Xform \"Capsule\""), "{text}");
        assert!(text.contains("def Capsule \"Proxy\""), "{text}");
        // Pose Xform seeded to identity (per-frame pose-write target).
        assert!(
            text.contains("uniform token[] xformOpOrder = [\"xformOp:transform\"]"),
            "{text}"
        );
        // Camera clip range comes straight from the params.
        assert!(
            text.contains("float2 clippingRange = (0.05, 10000)"),
            "{text}"
        );
    }

    #[test]
    fn player_head_block_carries_eye_height_and_wraps_camera() {
        let text = player_head_block(PAWN_CAMERA_DEFAULT_FOV_Y, 0.05, 10_000.0, "    ");
        assert!(text.contains("def Xform \"Head\""), "{text}");
        // Eye height offset up the +Y axis.
        assert!(
            text.contains("double3 xformOp:translate = (0, 1.5, 0)"),
            "{text}"
        );
        // The camera lives inside the Head.
        assert!(text.contains("def Camera \"Camera\""), "{text}");
    }

    #[test]
    fn player_camera_focal_length_matches_vertical_fov() {
        // 60° vertical FOV, 24mm vertical aperture → focalLength ≈ 20.78.
        let text = player_camera_block(PAWN_CAMERA_DEFAULT_FOV_Y, 0.1, 100.0, "");
        let expected =
            (PAWN_CAMERA_VERTICAL_APERTURE / 2.0) / (PAWN_CAMERA_DEFAULT_FOV_Y / 2.0).tan();
        assert!(
            text.contains(&format!("float focalLength = {expected}")),
            "expected focalLength {expected} in:\n{text}"
        );
        assert!(
            text.contains("token projection = \"perspective\""),
            "{text}"
        );
    }

    #[test]
    fn player_capsule_stands_on_ground_with_correct_dimensions() {
        let text = player_capsule_block("    ");
        // Cylinder segment = total − 2·radius = 1.7 − 0.5 = 1.2.
        assert!(text.contains("double height = 1.2"), "{text}");
        assert!(text.contains("double radius = 0.25"), "{text}");
        assert!(text.contains("uniform token axis = \"Y\""), "{text}");
        // Centered at half-height so the base sits at y = 0.
        assert!(
            text.contains("double3 xformOp:translate = (0, 0.85, 0)"),
            "{text}"
        );
    }

    #[test]
    fn player_over_world_layer_wraps_player_in_over_world() {
        let text = player_over_world_layer(None, 0.05, 10_000.0);
        assert!(text.starts_with("over \"World\"\n{"), "{text}");
        assert!(text.contains("def Xform \"Player\""), "{text}");
        // No bootstrap `def Scope` needed — /World is already defined elsewhere.
        assert!(!text.contains("def Scope"), "{text}");
    }

    #[test]
    fn model_over_layer_authors_over_chain_onto_existing_pose_xforms() {
        // The packaged-runtime injection must reach the already-authored pose
        // Xforms with an `over` chain — never a `def` that would re-create the
        // rig — and hang the resolved device Model beneath each hand.
        let left = ControllerAsset {
            asset_path: "/pkg/assets/quest3_left.usdc".to_string(),
            settings: CoordinateSettings::WORLD,
            mount_offset: None,
        };
        let right = ControllerAsset {
            asset_path: "/pkg/assets/quest3_right.usdc".to_string(),
            settings: CoordinateSettings::WORLD,
            mount_offset: None,
        };
        let text =
            controller_model_over_layer(Some(&left), Some(&right), CoordinateSettings::WORLD);
        // Pure `over` composition — no rig re-definition.
        assert!(text.contains("over \"World\""), "{text}");
        assert!(text.contains("over \"Player\""), "{text}");
        assert!(text.contains("over \"Controllers\""), "{text}");
        assert!(text.contains("over \"Left\""), "{text}");
        assert!(text.contains("over \"Right\""), "{text}");
        assert!(
            !text.contains("def Xform \"Player\"") && !text.contains("def Xform \"Left\""),
            "injection layer must not re-define the rig: {text}"
        );
        // The Model reference nodes ARE defined (they don't exist in the packaged
        // rig until the device is resolved at startup).
        assert_eq!(
            text.matches("def Xform \"Model\"").count(),
            2,
            "one Model per hand: {text}"
        );
        assert!(
            text.contains("prepend references = @/pkg/assets/quest3_left.usdc@"),
            "{text}"
        );
        assert!(
            text.contains("prepend references = @/pkg/assets/quest3_right.usdc@"),
            "{text}"
        );
    }

    #[test]
    fn model_over_layer_carries_mount_and_normalization() {
        // The injected Model must carry the same mount + up-axis/units
        // normalization the preview sublayer authors — byte-identical inner body
        // to `controller_model_block`.
        let asset = ControllerAsset {
            asset_path: "/pkg/assets/questpro.usdc".to_string(),
            settings: CoordinateSettings::from_usd("Z", 0.01),
            mount_offset: Some(MountOffset {
                rotate_xyz: [180.0, 0.0, 0.0],
                translate: [0.0, 0.0, 0.0],
                scale: [1.0, 1.0, 1.0],
            }),
        };
        let text = controller_model_over_layer(Some(&asset), None, CoordinateSettings::WORLD);
        // Only the resolved (left) hand gets an over; the unresolved hand is
        // skipped (capsule fallback applies at runtime).
        assert!(text.contains("over \"Left\""), "{text}");
        assert!(!text.contains("over \"Right\""), "{text}");
        // Mount wraps Model; normalization (Z-up → -90 X, 0.01 MPU → 100 scale)
        // rides the inner Model.
        let mount_at = text.find("def Xform \"Mount\"").expect("Mount node");
        let model_at = text.find("def Xform \"Model\"").expect("Model node");
        assert!(mount_at < model_at, "Mount must wrap Model: {text}");
        assert!(
            text.contains("double3 xformOp:rotateXYZ = (180, 0, 0)"),
            "{text}"
        );
        assert!(
            text.contains("double3 xformOp:rotateXYZ = (-90, 0, 0)"),
            "{text}"
        );
    }

    #[test]
    fn model_over_layer_empty_when_no_asset_resolves() {
        // No resolved device on either hand → no injection layer at all (the
        // runtime capsule fallback covers both hands).
        let text = controller_model_over_layer(None, None, CoordinateSettings::WORLD);
        assert!(text.is_empty(), "expected empty layer, got: {text:?}");
    }

    #[test]
    fn model_over_block_matches_pose_block_inner_body() {
        // The over-layer's inner Model body must be byte-identical to the body
        // `controller_pose_block` authors, so preview and packaged runtime place
        // the device identically. Compare at the shared 16-space indent.
        let asset = ControllerAsset {
            asset_path: "/a/dev.usdc".to_string(),
            settings: CoordinateSettings::from_usd("Z", 1.0),
            mount_offset: Some(MountOffset {
                rotate_xyz: [90.0, 0.0, 0.0],
                translate: [0.1, 0.0, 0.0],
                scale: [1.0, 1.0, 1.0],
            }),
        };
        let inner = controller_model_block(&asset, CoordinateSettings::WORLD, "            ");
        let pose = controller_pose_block("Left", Some(&asset), CoordinateSettings::WORLD);
        assert!(
            pose.contains(&inner),
            "pose block must embed the shared model block verbatim:\n--- inner ---\n{inner}\n--- pose ---\n{pose}"
        );
    }
}
