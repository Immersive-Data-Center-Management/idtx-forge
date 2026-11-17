//! Runtime-preview **assembly-root composition**.
//!
//! When the editor launches a runtime preview it does not hand `idtx-runtime`
//! the authored `world.usda` directly. Instead it synthesizes a thin *assembly
//! root layer* whose sublayer stack is composed from the current [`RunMode`] and
//! the [`RuntimeManifest`]:
//!
//! ```text
//! assembly-root.usda            (generated; strongest local opinions)
//!   subLayers = [
//!     player.usda               (XR mode only — the rig-embedded player prefab
//!                                under /World/Player: camera + capsule +
//!                                Controllers/Pointers)
//!     <world.usda>              (the playable world open in the editor; weakest)
//!   ]
//! ```
//!
//! The world is untouched: mode-specific capability layers ride *above* it as
//! sublayers, exactly as the layering proposal describes. The controller mesh
//! asset paths are resolved through the [`RuntimeManifest`] search roots, so the
//! meshes can ship in a packaged bundle or be dropped in by a vendor later.
//!
//! Everything is authored as text into a temp dir with **absolute** sublayer
//! paths, so the assembly is location-independent while `world.usda`'s own
//! relative references still resolve against its real directory. Every step logs
//! verbosely to stderr (`[preview]`), and the final assembly layer is dumped to
//! the console as text — the *layer*, not a flattened composition — for
//! inspection.

use std::io;
use std::path::{Path, PathBuf};

use idtx_runtime_manifest::{MountConfig, RuntimeManifest};
use idtx_studio_session::StudioSession;
use idtx_studio_session::xr_controller_prefab::{
    ControllerAsset, CoordinateSettings, MountOffset, player_rig_over_world_layer,
};
use idtx_usd::{ShadowMode, StageService};

#[cfg(feature = "authoring")]
use crate::assembly::{AssemblyCfg, author_assembly_stage, plan_assembly};

/// The preview run mode; selects which capability layers are mounted and maps to
/// the runtime's `--mode` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewMode {
    /// XR mode: mount the XR controller-defaults layer above the world.
    Xr,
    /// Desktop mode: mount the world alone and preview in a plain window with
    /// the orbit camera and no `OpenXR` session. The runtime drives the
    /// same frame loop (behaviors included) as XR mode.
    Desktop,
}

impl PreviewMode {
    /// The `--mode` argument string the runtime binary parses.
    #[must_use]
    pub fn as_arg(self) -> &'static str {
        match self {
            PreviewMode::Xr => "xr",
            PreviewMode::Desktop => "desktop",
        }
    }
}

/// Temp files created for a preview, deleted when this guard drops: the assembly
/// temp dir (always) and a flattened world temp beside the source (dirty stages).
#[derive(Debug)]
pub struct PreviewTemp {
    dir: PathBuf,
    world_temp: Option<PathBuf>,
    /// The behavior-module overlay stage (authoring builds only), kept alive so
    /// its temp file survives until the preview process exits.
    #[cfg(feature = "authoring")]
    _behavior_overlay: Option<crate::behavior_prepare::AuthoredOverlay>,
}

impl PreviewTemp {
    #[cfg(feature = "authoring")]
    pub(crate) fn prepare_template_manifest(
        &self,
        base: &RuntimeManifest,
        ctx: Option<&BehaviorPrepareCtx>,
    ) -> io::Result<PathBuf> {
        let mut manifest = base.clone();
        manifest.runtime_prototypes.clear();
        manifest.network = Default::default(); // Project policy is passed via CLI.
        manifest.experience = Default::default(); // Actual preview stage is passed via CLI.
        let cwd = std::env::current_dir()?;
        let absolute = |path: PathBuf| if path.is_absolute() { path } else { cwd.join(path) };
        manifest.assets.search_paths = base.asset_search_roots().into_iter().map(absolute).collect();
        for (id, entry) in &mut manifest.xr.controllers {
            if let Some(resolved) = base.resolve_controller_asset(id) {
                if resolved.device == *id {
                    entry.left = Some(absolute(resolved.left));
                    entry.right = Some(absolute(resolved.right));
                    entry.asset = None;
                }
            }
        }
        if let Some(ctx) = ctx {
            let project = idtx_project::IdtxProject::load(ctx.project_root.join("project.toml")).map_err(io::Error::other)?;
            manifest.runtime_prototypes = idtx_build::prototype_prepare::prepare_runtime_prototypes(
                &project, ctx.sdk_path.as_deref(), &self.dir.join("runtime_prototypes"),
            ).map_err(io::Error::other)?;
        }
        let path = self.dir.join("run.toml");
        std::fs::write(&path, manifest.to_toml_string())?;
        Ok(path)
    }
}

impl Drop for PreviewTemp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        if let Some(world) = &self.world_temp {
            let _ = std::fs::remove_file(world);
        }
    }
}

/// Inputs for the compile-behaviors-before-preview pass (authoring builds only).
///
/// The world stage the preview composes may carry authored `idtx:behavior:source`
/// prims whose `.wasm` has not been built yet (or is stale). [`build_assembly`]
/// compiles those sources into `<project_root>/behaviors` and links the modules
/// into a temp overlay used as the world sublayer — the source stage is never
/// mutated. `sdk_path` is the staged guest-ABI SDK sysroot, or `None`.
#[derive(Debug, Clone)]
pub struct BehaviorPrepareCtx {
    pub project_root: PathBuf,
    pub sdk_path: Option<PathBuf>,
}

/// Project-driven preview composition: compose the project's level(s) as
/// the world with the mode-selected player overlay as the strongest sublayer,
/// instead of wrapping the open editor tab (whose baked `assembly.usda` pins a
/// single player, producing a double-player and the wrong-mode behaviors).
///
/// This is the ephemeral twin of the persistent [`crate::assembly`] author: it
/// composes exactly *one* player — the one matching the target [`PreviewMode`] —
/// so the desktop FPS/pitch behaviors never leak into an XR preview and the XR
/// controller/aim behaviors never leak into desktop. Controllers are injected by
/// the runtime over the bare rig (the single source of truth for the
/// out-of-process path), so this path bakes no device meshes.
#[derive(Debug, Clone)]
pub struct PlayerComposition {
    /// The player stage composed as a reference at `/World/Player` (a project
    /// stage, or the toolbox built-in). `None` when the project configures no
    /// player — the levels then compose alone (no player prim).
    pub player: Option<crate::assembly::PlayerRef>,
    /// The project's level stages (absolute), in registry order, composed beneath
    /// the player as the world.
    pub levels: Vec<PathBuf>,
}

/// Build the assembly-root layer for `session` and return `(assembly_path, temp)`.
/// Pass the returned path to `idtx-runtime --stage`; keep `temp` alive until the
/// preview process exits so the generated files are not deleted underneath it.
///
/// `controller_device` selects the manifest entry for XR controller meshes
/// (e.g. `"questpro"`, `"quest3"`); falls back to `"generic"` when `None`.
/// The manifest in turn falls back to its own `"generic"` entry when the given
/// device has no registered asset, so unknown devices always get a mesh or the
/// runtime capsule fallback. `controller_device` is only consulted on the legacy
/// open-tab path (which bakes meshes); on the [`PlayerComposition`] project path
/// the runtime injects controllers over the bare rig instead.
///
/// When `player` carries a project composition with at least one level, the
/// assembly is composed from the project's level(s) + the mode-selected player
/// overlay ([`build_project_assembly`]); otherwise the legacy open-tab path wraps
/// `session`'s current world (loose-file preview).
///
/// # Errors
///
/// Fails when the world stage cannot be resolved/flattened or the generated
/// layers cannot be written.
#[cfg(feature = "authoring")]
pub fn build_assembly(
    session: &StudioSession,
    dirty: bool,
    mode: PreviewMode,
    manifest: &RuntimeManifest,
    controller_device: Option<&str>,
    player: Option<&PlayerComposition>,
    behavior_ctx: Option<&BehaviorPrepareCtx>,
) -> io::Result<(PathBuf, PreviewTemp)> {
    #[cfg(feature = "preview-metrics")]
    let started = std::time::Instant::now();

    eprintln!(
        "[preview] composing assembly-root layer (mode = {})",
        mode.as_arg()
    );
    eprintln!(
        "[preview] runtime manifest v{} (hash {})",
        manifest.version,
        manifest.content_hash()
    );

    /*
    // Project preview path: compose the project's level(s) + the
    // mode-selected player overlay directly, rather than wrapping the open tab's
    // baked assembly (which pins one player and would double up the controllers
    // and cross-wire the wrong mode's behaviors). Falls through to the legacy
    // open-tab path when no project composition is supplied (loose-file preview)
    // or it carries no levels.
    if let Some(comp) = player.filter(|c| !c.levels.is_empty()) {
        return build_project_assembly(session, mode, comp, behavior_ctx);
    }

    */
    if let Some(comp) = player {
        return build_project_assembly(session, mode, comp, behavior_ctx, dirty);
    }

    eprintln!("[preview] No Player Composition available");

    // 1. Resolve the world layer — the playable stage open in the editor.
    let (world_path, world_temp) = resolve_world(session, dirty)?;

    // 1b. Compile any authored behavior sources on the (possibly flattened) world
    //     and link the modules into a temp overlay used as the world layer. The
    //     authored source stage is never mutated. Fail-closed: a compile error
    //     aborts the preview rather than launching a player with a missing module.
    #[cfg(feature = "authoring")]
    let (world_path, behavior_overlay) = apply_behavior_overlay(&world_path, behavior_ctx)?;

    let world_asset = usd_asset_path(&world_path);
    eprintln!(
        "[preview]   world layer: {}{}",
        world_asset,
        if world_temp.is_some() {
            "  (flattened from unsaved edits)"
        } else {
            ""
        }
    );

    // 2. Create the assembly temp dir under the project root (so the runtime
    //    resolves project-subfolder references and the real `project.toml`).
    let project_root = source_project_root(session.source_path(), behavior_ctx);
    let dir = make_temp_dir(project_root.as_deref())?;
    eprintln!(
        "[preview]   temp dir:   {}",
        idtx_project::normalize_display_path(&dir)
    );

    // 2b. Copy the project's `input_map.toml` beside the generated assembly.
    //     The out-of-process runtime resolves the input map from its *stage's*
    //     project root, but the stage we hand it is `assembly-root.usda` in this
    //     temp dir — which has no `project.toml`, so the runtime's project-root
    //     walk falls back to the temp dir itself. Without this copy it finds no
    //     map and authored input bindings (WASD/mouse) go dead. A missing source
    //     map is a no-op (the runtime's own no-bindings path applies).
    copy_input_map_into(&dir, session.source_path(), behavior_ctx);

    // 3. In XR mode, generate the controller-defaults layer above the world.
    let mut sublayers: Vec<String> = Vec::new();
    if mode == PreviewMode::Xr {
        let device = controller_device.unwrap_or("generic");
        let resolved = manifest.resolve_controller_asset(device);
        match &resolved {
            Some(rc) => eprintln!(
                "[preview]   xr controllers ({} → manifest \"{}\"): left={}  right={}",
                device,
                rc.device,
                rc.left.display(),
                rc.right.display()
            ),
            None => eprintln!(
                "[preview]   xr controllers: none resolved for device \"{}\" over roots {:?}; \
                 mounting empty rig (runtime capsule fallback applies)",
                device,
                manifest.asset_search_roots()
            ),
        }
        // Derive each controller's metre / Y-up correction from its *own* root
        // metadata (independent of the assembly-root's metersPerUnit). The
        // world stage's projection_basis is deliberately never applied to the
        // controllers — the pose Xform receives the render-space grip pose
        // directly, and the inner Model normalizes the asset into that space.
        #[cfg(feature = "preview-metrics")]
        let corr_started = std::time::Instant::now();
        let model_layer = resolved.as_ref().map(|rc| {
            let mount = rc.mount.map(mount_offset_from_config);
            if let Some(m) = mount {
                eprintln!(
                    "[preview]   controller mount offset (device \"{}\", from run.toml): rotateXYZ={:?} translate={:?} scale={:?}",
                    rc.device, m.rotate_xyz, m.translate, m.scale
                );
            }
            ControllerModelLayer {
                left: ControllerAsset {
                    asset_path: usd_asset_path(&rc.left),
                    settings: read_controller_settings(&rc.left),
                    mount_offset: mount,
                },
                right: ControllerAsset {
                    asset_path: usd_asset_path(&rc.right),
                    settings: read_controller_settings(&rc.right),
                    mount_offset: mount,
                },
            }
        });
        #[cfg(feature = "preview-metrics")]
        eprintln!(
            "[preview-metrics] read controller settings in {:?}",
            corr_started.elapsed()
        );

        let xr_path = dir.join("player.usda");
        std::fs::write(&xr_path, xr_layer_text(model_layer.as_ref()))?;
        eprintln!(
            "[preview]   player layer: {}",
            idtx_project::normalize_display_path(&xr_path)
        );
        sublayers.push(usd_asset_path(&xr_path));
    }

    // World is always the weakest sublayer.
    sublayers.push(world_asset);

    // 4. Author the assembly root (carry world stage metadata onto the root —
    //    `GetDefaultPrim`, `upAxis` and `idtx:shadowMode` are all read from the
    //    root layer only, so they must be propagated onto the generated root).
    let meta = session.metadata();
    // The shadow mode reads from the root layer's `customLayerData` only (never
    // the composed metadata), so the world's authored tier does not survive the
    // sublayer stack — it must be re-authored onto the assembly root or the
    // runtime resolves shadows to OFF. Take the session's *live* mode (honours a
    // just-authored change the cached `StageMetadata` has not picked up).
    let assembly_text = assembly_layer_text(
        meta.default_prim.as_deref(),
        &meta.up_axis,
        meta.meters_per_unit,
        session.stage_shadow_mode(),
        &sublayers,
    );
    let assembly_path = dir.join("assembly-root.usda");
    std::fs::write(&assembly_path, &assembly_text)?;

    #[cfg(feature = "preview-metrics")]
    eprintln!(
        "[preview-metrics] authored assembly in {:?}",
        started.elapsed()
    );

    // 5. Validate the composition and dump the layer text for inspection.
    dump_assembly(&assembly_path, &assembly_text);

    #[cfg(feature = "preview-metrics")]
    eprintln!(
        "[preview-metrics] total assembly build {:?}",
        started.elapsed()
    );

    Ok((
        assembly_path,
        PreviewTemp {
            dir,
            world_temp,
            #[cfg(feature = "authoring")]
            _behavior_overlay: behavior_overlay,
        },
    ))
}

/// Compose a preview assembly from the project's level(s) + the mode-selected
/// player overlay. See [`PlayerComposition`].
///
/// The sublayer stack is `[player_stage (strongest), levels... (weakest)]`, so
/// `/World` content comes from the levels and `/World/Player` from the player
/// overlay. Controllers are injected by the runtime over the bare rig — nothing
/// is baked here, so an XR preview shows exactly one (real) controller pair
/// rather than a baked generic pair *plus* the runtime-injected real pair.
///
/// Behavior prep runs against the *composed* assembly root, so only the
/// mode-selected player's behaviors compile+link; the other mode's behaviors are
/// structurally absent (not merely unattached).
#[cfg(feature = "authoring")]
fn build_project_assembly(
    session: &StudioSession,
    mode: PreviewMode,
    comp: &PlayerComposition,
    behavior_ctx: Option<&BehaviorPrepareCtx>,
    dirty: bool,
) -> io::Result<(PathBuf, PreviewTemp)> {
    eprintln!(
        "[preview] composing project assembly (mode = {}): level(s) + mode-selected player",
        mode.as_arg()
    );

    let dir = make_temp_dir(source_project_root(session.source_path(), behavior_ctx).as_deref())?;
    eprintln!(
        "[preview]   temp dir:   {}",
        idtx_project::normalize_display_path(&dir)
    );

    // Copy the project's `input_map.toml` beside the generated assembly (same
    // reasoning as the legacy path: the runtime resolves the map from its stage's
    // project root, which for the handed-off assembly is this temp dir).
    copy_input_map_into(&dir, session.source_path(), behavior_ctx);

    let level_paths = if let Ok((world_path, world_temp)) = resolve_world(session, dirty) {
        vec![world_path.clone()]
    } else {
        comp.levels.clone()
    };

    eprintln!("[preview]   prepare assembly");
    let mut assembly_plan = plan_assembly(dir.as_path(), &level_paths, comp.player.as_ref());
    // Select the player rig variant for the preview mode (XR → "xr", else
    // "desktop").
    assembly_plan.player_variant = Some(
        match mode {
            PreviewMode::Xr => "xr",
            PreviewMode::Desktop => "desktop",
        }
        .to_owned(),
    );
    let meta = session.metadata();
    let assembly_cfg = AssemblyCfg {
        up_axis: meta.up_axis.clone(),
        meters_per_unit: meta.meters_per_unit,
        shadow_mode: session.stage_shadow_mode(),
    };
    eprintln!("[preview]   prepare assembly - plan created - now author");
    let assembly_path = dir.join("assembly-root.usda");
    author_assembly_stage(
        assembly_path.as_path(),
        &assembly_plan,
        Some(&assembly_cfg),
        "Preview",
    )
    .map_err(|e| {
        eprintln!("[preview] Unable to save assebly stage. Error: {e:?}");
        std::io::Error::other(e)
    })?;

    eprintln!("[preview]   assembly authored and saved");

    // Compile authored behaviors across the composed player + levels and link the
    // modules into an overlay over the assembly root (authoring builds). Only the
    // mode-selected player is composed, so the other mode's behaviors never
    // appear. Fail-closed: a compile error aborts the preview.
    #[cfg(feature = "authoring")]
    let (stage_path, behavior_overlay) = apply_behavior_overlay(&assembly_path, behavior_ctx)?;
    #[cfg(not(feature = "authoring"))]
    let stage_path = assembly_path.clone();

    //dump_assembly(&assembly_path, &assembly_text);

    Ok((
        stage_path,
        PreviewTemp {
            dir,
            world_temp: None,
            #[cfg(feature = "authoring")]
            _behavior_overlay: behavior_overlay,
        },
    ))
}

/// Compile authored behavior sources on `world_path` and, when any exist, return
/// the temp overlay stage (linking `idtx:behavior:module` per prim) to use as the world
/// layer plus its lifetime guard. With no `ctx` or no sources, returns the world
/// path unchanged and no guard.
#[cfg(feature = "authoring")]
fn apply_behavior_overlay(
    world_path: &Path,
    ctx: Option<&BehaviorPrepareCtx>,
) -> io::Result<(PathBuf, Option<crate::behavior_prepare::AuthoredOverlay>)> {
    let Some(ctx) = ctx else {
        eprintln!("[preview] No BehaviorPrepareCtx passed.");
        return Ok((world_path.to_path_buf(), None));
    };
    let prepared =
        crate::behavior_prepare::prepare(world_path, &ctx.project_root, ctx.sdk_path.as_deref())
            .map_err(io::Error::other)?;
    if prepared.is_empty() {
        eprintln!("[preview] No behavior prepared.");
        return Ok((world_path.to_path_buf(), None));
    }
    eprintln!(
        "[preview]   compiled {} behavior source(s); linking modules into overlay",
        prepared.items.len()
    );
    let overlay = crate::behavior_prepare::author_module_overlay(world_path, &prepared)
        .map_err(io::Error::other)?;
    Ok((overlay.path().to_path_buf(), Some(overlay)))
}

/// Resolve the world layer path, flattening a temp beside the source when the
/// tab has unsaved edits (so relative references/textures still resolve).
fn resolve_world(session: &StudioSession, dirty: bool) -> io::Result<(PathBuf, Option<PathBuf>)> {
    let source: &Path = session.source_path();
    let absolute = std::path::absolute(source).unwrap_or_else(|_| source.to_path_buf());
    if !dirty {
        return Ok((absolute, None));
    }

    #[cfg(feature = "authoring")]
    {
        let dir = source.parent().unwrap_or_else(|| Path::new("."));
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let temp = dir.join(format!(
            ".idtx-preview-{}-{}.usdc",
            std::process::id(),
            nanos
        ));
        session
            .export_flattened_stage(&temp)
            .map_err(|e| io::Error::other(format!("flatten stage: {e}")))?;
        let temp_abs = std::path::absolute(&temp).unwrap_or_else(|_| temp.clone());
        return Ok((temp_abs, Some(temp)));
    }
    #[cfg(not(feature = "authoring"))]
    Ok((absolute, None))
}

/// Open the assembly topology-only to confirm it composes, log the composed
/// layer stack, and dump the assembly *layer* text (not flattened) to stderr.
fn dump_assembly(assembly_path: &Path, assembly_text: &str) {
    match StageService::open_topology_only(assembly_path) {
        Ok(stage) => match stage.layer_stack() {
            Ok(layers) => {
                eprintln!(
                    "[preview] composition OK — {} layer(s) in stack (strongest first):",
                    layers.len()
                );
                for layer in &layers {
                    eprintln!(
                        "[preview]     [{:?}] {}{}",
                        layer.arc,
                        layer.display_name,
                        if layer.is_anonymous {
                            "  (anonymous)"
                        } else {
                            ""
                        }
                    );
                }
            }
            Err(e) => eprintln!("[preview] composition opened but layer_stack failed: {e}"),
        },
        Err(e) => eprintln!("[preview] WARNING: assembly failed to open topology-only: {e}"),
    }

    eprintln!(
        "[preview] ── assembly-root layer ({}): ──",
        idtx_project::normalize_display_path(assembly_path)
    );
    for line in assembly_text.lines() {
        eprintln!("[preview] | {line}");
    }
    eprintln!("[preview] ── end assembly-root layer ──");
}

/// A resolved controller pair with each asset's own coordinate settings, ready
/// to author as **visible** referenced pose blocks.
struct ControllerModelLayer {
    left: ControllerAsset,
    right: ControllerAsset,
}

/// Open a controller asset topology-only and read its own coordinate settings
/// (`upAxis` / `metersPerUnit`) so the pose block can normalize it into the
/// render/XR world (metre, Y-up) via [`controller_pose_block`].
///
/// Falls back to [`CoordinateSettings::WORLD`] (identity — no correction) when
/// the asset cannot be opened for metadata, matching the prior fail-safe of
/// authoring the model without an up-axis/units correction.
fn read_controller_settings(asset: &Path) -> CoordinateSettings {
    match StageService::open_topology_only(asset) {
        Ok(stage) => {
            let meta = stage.metadata();
            let settings = CoordinateSettings::from_usd(&meta.up_axis, meta.meters_per_unit);
            eprintln!(
                "[preview]   controller {} coord settings (upAxis={}, metersPerUnit={})",
                idtx_project::normalize_display_path(asset),
                meta.up_axis,
                meta.meters_per_unit
            );
            settings
        }
        Err(e) => {
            eprintln!(
                "[preview]   WARNING: controller asset {} did not open for metadata ({e}); \
                 placing model without up-axis/units correction",
                idtx_project::normalize_display_path(asset)
            );
            CoordinateSettings::WORLD
        }
    }
}

/// Convert a manifest [`MountConfig`] (TRS, from `run.toml`) into the prefab
/// [`MountOffset`] the pose-block generator consumes. Absent translate/rotate
/// resolve to `[0,0,0]`; absent scale resolves to unit `[1,1,1]` (a unit scale
/// then authors no `xformOp:scale`, keeping the model byte-identical).
///
/// `run.toml` is the single source for the offset — there is no built-in
/// per-device default and no env override; a device with no `mount` block gets
/// no offset (placed by the generic up-axis/units normalization only).
fn mount_offset_from_config(cfg: MountConfig) -> MountOffset {
    MountOffset {
        rotate_xyz: cfg.rotate(),
        translate: cfg.translate(),
        scale: cfg.scale(),
    }
}

/// asset is authored as a **visible** pose Xform
/// (`/World/Player/Controllers/{Left,Right}`) with an inner, normalized `Model`
/// referencing the asset; otherwise bare visible pose Xforms are mounted (no
/// geometry, but valid per-frame pose-write targets).
///
/// The controllers flow through the ordinary
/// `project_viewport → hydrator → scene_extract` path — skeleton, GeomSubset
/// materials, and authored transform intact — instead of the legacy invisible
/// proxy + runtime mesh bake. The per-frame `OpenXR` grip pose is written onto the
/// outer pose Xform via the session-layer transform channel
/// (`StudioSession::author_hydrated_controller_poses`), so moving the pose Xform
/// drives the whole (skinned or rigid) controller. The pose block is generated
/// by `xr_controller_prefab::controller_pose_block`, shared verbatim with the
/// session-layer prefab path.
///
/// This generates the full **rig-embedded player prefab**: the generated layer
/// is a single `over "World" { def Xform "Player" {
/// Camera + Capsule + Controllers/{Left,Right} + Pointers/{Left,Right} } }`
/// block (via [`player_rig_over_world_layer`]). The controllers/pointers are
/// children of the already-`def` `/World/Player`, so — unlike the retired global
/// `/XR` layout — **no** `def Scope "XR"` root-layer bootstrap is needed to
/// survive topology traversal.
fn xr_layer_text(controllers: Option<&ControllerModelLayer>) -> String {
    let world = CoordinateSettings::WORLD;
    let (left, right) = match controllers {
        Some(m) => (Some(&m.left), Some(&m.right)),
        None => (None, None),
    };
    // Pawn root: the camera FOV/clip default here; a live HMD FOV override can
    // be threaded through later without changing the layer shape.
    let player = player_rig_over_world_layer(None, 0.05, 10_000.0, left, right, world);
    format!(
        "#usda 1.0\n(\n    doc = \"IDTX runtime-preview player prefab (generated)\"\n)\n\n{player}"
    )
}

/// Serialize the assembly root: world stage metadata + the sublayer stack.
///
/// The rig-embedded player prefab nests the controllers/pointers under the
/// already-`def` `/World/Player`, so — unlike the retired global `/XR` layout —
/// **no** root-layer `def Scope "XR"` bootstrap is needed to keep the controller
/// subtree alive through the `IsDefined` topology-traversal predicate.
fn assembly_layer_text(
    default_prim: Option<&str>,
    up_axis: &str,
    meters_per_unit: f64,
    shadow_mode: ShadowMode,
    sublayers: &[String],
) -> String {
    let mut header = String::from(
        "#usda 1.0\n(\n    doc = \"IDTX runtime-preview assembly root (generated; do not edit)\"\n",
    );
    if let Some(prim) = default_prim {
        // `defaultPrim` metadata names a *root prim* — a bare name, not an
        // absolute path. The bridge reports it as `GetPath()` ("/World"), so
        // strip the leading slash (and any nested tail) back to the root name;
        // otherwise `UsdStage::GetDefaultPrim()` fails to resolve it and the
        // runtime opens a stage with no default prim and exits immediately.
        if let Some(name) = root_prim_name(prim) {
            header.push_str(&format!("    defaultPrim = \"{name}\"\n"));
        }
    }
    if !up_axis.is_empty() {
        header.push_str(&format!("    upAxis = \"{up_axis}\"\n"));
    }
    header.push_str(&format!("    metersPerUnit = {meters_per_unit}\n"));
    header.push_str("    subLayers = [\n");
    for (i, path) in sublayers.iter().enumerate() {
        let comma = if i + 1 < sublayers.len() { "," } else { "" };
        header.push_str(&format!("        @{path}@{comma}\n"));
    }
    header.push_str("    ]\n)\n");
    header
}

/// The `idtx:shadowMode` token authored into `customLayerData` for a mode, or
/// `None` for [`ShadowMode::Off`] (the default when the key is absent, so OFF is
/// authored by omission — keeping the shadowless assembly byte-identical to
/// before this propagation landed). Tokens mirror `author_root_shadow_mode` in
/// the C++ shim: `hard`/`smooth`/`soft`.
fn shadow_mode_token(mode: ShadowMode) -> Option<&'static str> {
    match mode {
        ShadowMode::Off => None,
        ShadowMode::Hard => Some("hard"),
        ShadowMode::Smooth => Some("smooth"),
        ShadowMode::Soft => Some("soft"),
    }
}

/// Convert a filesystem path to a USD asset-path string (forward slashes).
///
/// Strips any Windows verbatim (`\\?\`) prefix first: a canonicalized path
/// forward-slashed to `//?/C:/…` is a valid *direct* USD layer identifier but
/// breaks relative-reference anchoring inside the referenced layer.
fn usd_asset_path(path: &Path) -> String {
    idtx_project::strip_verbatim_prefix(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The root-prim name for a `defaultPrim` value that may arrive as an absolute
/// path ("/World" → "World") or already as a bare name ("World" → "World").
/// Returns `None` for an empty/degenerate value so no `defaultPrim` is authored.
fn root_prim_name(default_prim: &str) -> Option<&str> {
    default_prim
        .trim_start_matches('/')
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
}

/// Create a unique temp dir for one preview's generated layers.
///
/// When `project_root` is known the scratch dir is created **under it** (a hidden
/// `.idtx-preview-assembly-*` dir, removed by [`PreviewTemp`] on drop). This is
/// what lets the out-of-process preview runtime resolve references that point at
/// project subfolders: the generated assembly's relative level-payload arcs
/// resolve against the project tree, and the runtime's `project.toml` ancestor
/// walk from the assembly finds the real project root (so `input_map.toml` and
/// the asset search path resolve against the project, not a disconnected system
/// temp dir). Falls back to the system temp dir when the project root is unknown.
fn make_temp_dir(project_root: Option<&Path>) -> io::Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = format!(".idtx-preview-assembly-{}-{nanos}", std::process::id());
    let base = project_root.map_or_else(std::env::temp_dir, Path::to_path_buf);
    let dir = base.join(name);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Resolve the world source's project root: prefer the open project's root
/// (authoritative, authoring builds), else the nearest ancestor of the world
/// `source` holding a `project.toml`, falling back to the source's own
/// directory. The fallback walk mirrors the runtime's `project_root_for_stage`,
/// so the input map the editor copies is exactly the one the runtime would
/// resolve for the world itself.
fn source_project_root(
    source: &Path,
    behavior_ctx: Option<&BehaviorPrepareCtx>,
) -> Option<PathBuf> {
    if let Some(ctx) = behavior_ctx {
        return Some(ctx.project_root.clone());
    }
    let start = source.parent()?;
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("project.toml").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    Some(start.to_path_buf())
}

/// The project's `input_map.toml` for the world `source`, or `None` when the
/// project root cannot be resolved or the file does not exist.
fn resolve_project_input_map(
    source: &Path,
    behavior_ctx: Option<&BehaviorPrepareCtx>,
) -> Option<PathBuf> {
    let map = source_project_root(source, behavior_ctx)?.join("input_map.toml");
    map.is_file().then_some(map)
}

/// Copy the project's `input_map.toml` (if any) into the assembly temp `dir` as
/// `input_map.toml`, so the out-of-process runtime — which resolves the input
/// map from its stage's project root, and whose stage lives in `dir` — finds the
/// project's bindings. A missing source map or a copy failure is logged and left
/// non-fatal; the runtime's own missing-map path then applies.
fn copy_input_map_into(dir: &Path, source: &Path, behavior_ctx: Option<&BehaviorPrepareCtx>) {
    let Some(src) = resolve_project_input_map(source, behavior_ctx) else {
        eprintln!("[preview]   input map: none at project root; runtime runs with no bindings");
        return;
    };
    let dest = dir.join("input_map.toml");
    match std::fs::copy(&src, &dest) {
        Ok(_) => eprintln!(
            "[preview]   input map: {} → {}",
            src.display(),
            dest.display()
        ),
        Err(e) => eprintln!(
            "[preview]   WARNING: failed to copy input map {} → {} ({e}); runtime runs with no bindings",
            src.display(),
            dest.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_template_manifest_prepares_uninstantiated_assets_for_the_guard_lifetime() {
        let root = scratch_dir("preview-templates");
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/panel.usda"), "#usda 1.0\n( defaultPrim = \"Panel\" )\ndef Xform \"Panel\" {}\n").unwrap();
        let mut project_manifest = idtx_project::ProjectManifest::new("Templates");
        project_manifest.runtime_prototypes.insert("panel".into(), idtx_project::RuntimePrototypeConfig {
            asset: "assets/panel.usda".into(), prim: "/Panel".into(),
        });
        let project = idtx_project::IdtxProject::create(&root, project_manifest).unwrap();
        let ctx = BehaviorPrepareCtx { project_root: project.root().to_owned(), sdk_path: None };
        let dir = make_temp_dir(Some(&root)).unwrap();
        let guard = PreviewTemp { dir: dir.clone(), world_temp: None, _behavior_overlay: None };
        let manifest_path = guard.prepare_template_manifest(&RuntimeManifest::default(), Some(&ctx)).unwrap();
        let manifest = RuntimeManifest::load(&manifest_path).unwrap();
        let template = &manifest.runtime_prototypes["panel"];
        assert!(template.asset.is_absolute());
        assert!(template.asset.is_file());
        assert_eq!(template.prim, "/Panel");
        drop(guard);
        assert!(!dir.exists());
        drop(project); // Release the confined directory handle before Windows cleanup.
        std::fs::remove_dir_all(root).unwrap();
    }


    #[test]
    fn preview_mode_as_arg_matches_runtime_contract() {
        // The `--mode` string the editor spawns with must be exactly what
        // `idtx_runtime_core::RunMode::parse` maps back to the matching mode.
        // The runtime end is pinned by its own `parses_mode_case_insensitively`
        // test (`"xr"`→Xr, `"desktop"`→Desktop); this pins the editor end so the
        // two never drift across the process boundary.
        assert_eq!(PreviewMode::Xr.as_arg(), "xr");
        assert_eq!(PreviewMode::Desktop.as_arg(), "desktop");
    }

    #[test]
    fn root_prim_name_normalizes() {
        assert_eq!(root_prim_name("/World"), Some("World"));
        assert_eq!(root_prim_name("World"), Some("World"));
        assert_eq!(root_prim_name("/World/Child"), Some("World"));
        assert_eq!(root_prim_name("/"), None);
        assert_eq!(root_prim_name(""), None);
    }

    /// A stage nested under a project root resolves the project's `input_map.toml`
    /// by walking up to the `project.toml` marker — exactly as the runtime's own
    /// `project_root_for_stage` does, so the editor copies the map the runtime
    /// would otherwise have resolved for the world.
    #[test]
    fn resolve_input_map_walks_to_project_root() {
        let root = scratch_dir("input-map-walk");
        std::fs::write(root.join("project.toml"), "version = 1\n").unwrap();
        std::fs::write(root.join("input_map.toml"), "# bindings\n").unwrap();
        let stages = root.join("stages");
        std::fs::create_dir_all(&stages).unwrap();
        let world = stages.join("main.usda");
        std::fs::write(&world, "#usda 1.0\n").unwrap();

        let resolved = resolve_project_input_map(&world, None).expect("input map resolved");
        assert_eq!(resolved, root.join("input_map.toml"));

        std::fs::remove_dir_all(&root).ok();
    }

    /// A project with no `input_map.toml` resolves to `None` (the runtime then
    /// runs with no bindings) rather than pointing at a non-existent file.
    #[test]
    fn resolve_input_map_absent_is_none() {
        let root = scratch_dir("input-map-absent");
        std::fs::write(root.join("project.toml"), "version = 1\n").unwrap();
        let world = root.join("main.usda");
        std::fs::write(&world, "#usda 1.0\n").unwrap();

        assert!(resolve_project_input_map(&world, None).is_none());

        std::fs::remove_dir_all(&root).ok();
    }

    /// An open project's root takes precedence over the source walk: the input
    /// map is read from `behavior_ctx.project_root`, even when the world source
    /// sits elsewhere.
    #[test]
    fn resolve_input_map_prefers_behavior_ctx_root() {
        let project = scratch_dir("input-map-ctx-project");
        std::fs::write(project.join("input_map.toml"), "# ctx bindings\n").unwrap();
        let elsewhere = scratch_dir("input-map-ctx-world");
        let world = elsewhere.join("world.usda");
        std::fs::write(&world, "#usda 1.0\n").unwrap();

        let ctx = BehaviorPrepareCtx {
            project_root: project.clone(),
            sdk_path: None,
        };
        let resolved =
            resolve_project_input_map(&world, Some(&ctx)).expect("ctx input map resolved");
        assert_eq!(resolved, project.join("input_map.toml"));

        std::fs::remove_dir_all(&project).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
    }

    /// The copy lands the project map in the assembly temp dir as `input_map.toml`
    /// — the exact path the out-of-process runtime resolves from the handed-off
    /// stage's own directory.
    #[test]
    fn copy_input_map_lands_in_assembly_dir() {
        let root = scratch_dir("input-map-copy-src");
        std::fs::write(root.join("project.toml"), "version = 1\n").unwrap();
        std::fs::write(root.join("input_map.toml"), "# original bindings\n").unwrap();
        let world = root.join("main.usda");
        std::fs::write(&world, "#usda 1.0\n").unwrap();

        let assembly_dir = scratch_dir("input-map-copy-dest");
        copy_input_map_into(&assembly_dir, &world, None);

        let dest = assembly_dir.join("input_map.toml");
        assert!(dest.is_file(), "input map not copied into assembly dir");
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "# original bindings\n"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&assembly_dir).ok();
    }

    /// With no source map, the copy is a no-op — no stray `input_map.toml` is
    /// authored into the assembly dir (the runtime's own no-bindings path applies).
    #[test]
    fn copy_input_map_absent_writes_nothing() {
        let root = scratch_dir("input-map-noop-src");
        std::fs::write(root.join("project.toml"), "version = 1\n").unwrap();
        let world = root.join("main.usda");
        std::fs::write(&world, "#usda 1.0\n").unwrap();

        let assembly_dir = scratch_dir("input-map-noop-dest");
        copy_input_map_into(&assembly_dir, &world, None);

        assert!(!assembly_dir.join("input_map.toml").exists());

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&assembly_dir).ok();
    }

    #[test]
    fn xr_layer_references_resolved_controllers() {
        let model = ControllerModelLayer {
            left: ControllerAsset {
                asset_path: "/assets/left.usdc".into(),
                settings: CoordinateSettings::WORLD,
                mount_offset: None,
            },
            right: ControllerAsset {
                asset_path: "/assets/right.usdc".into(),
                settings: CoordinateSettings::WORLD,
                mount_offset: None,
            },
        };
        let text = xr_layer_text(Some(&model));
        assert!(text.contains("references = @/assets/left.usdc@"));
        assert!(text.contains("references = @/assets/right.usdc@"));
        assert!(text.contains("def Xform \"Controllers\""));
        // The reference must live on the inner Model node, not the pose Xform, so
        // the pose write drives the whole controller subtree.
        assert_eq!(
            text.matches("def Xform \"Model\"").count(),
            2,
            "both hands must reference via an inner Model node: {text}"
        );
        // The controllers hydrate visibly — no invisible proxy remnant.
        assert!(
            !text.contains("visibility = \"invisible\""),
            "hydrated controllers must not be authored invisible: {text}"
        );
        // Each hand carries a seeded identity pose transform (overwritten per
        // frame by the session-layer pose channel). Six total: two controller
        // pose Xforms + two pointer pose Xforms + the pawn-root `Player` Xform
        // and its `Camera` — all per-frame pose-write targets.
        assert_eq!(
            text.matches("matrix4d xformOp:transform = ( (1, 0, 0, 0)")
                .count(),
            6,
            "controllers + pointers + player rig each need a seeded pose transform: {text}"
        );
        // The player prefab leads with the pawn root (camera + capsule).
        assert!(text.contains("over \"World\""), "player over block: {text}");
        assert!(text.contains("def Xform \"Player\""), "pawn root: {text}");
        assert!(
            text.contains("def Camera \"Camera\""),
            "pawn camera: {text}"
        );
        assert!(
            text.contains("def Capsule \"Proxy\""),
            "capsule proxy: {text}"
        );
    }

    #[test]
    fn xr_layer_places_correction_on_model_node() {
        let model = ControllerModelLayer {
            left: ControllerAsset {
                asset_path: "/assets/zup.usdc".into(),
                settings: CoordinateSettings::from_usd("Z", 0.01),
                mount_offset: None,
            },
            right: ControllerAsset {
                asset_path: "/assets/zup.usdc".into(),
                settings: CoordinateSettings::from_usd("Z", 0.01),
                mount_offset: None,
            },
        };
        let text = xr_layer_text(Some(&model));
        // The normalization ops sit inside the Model block (after its reference),
        // never on the pose Xform — assert the Model node precedes the ops.
        let model_at = text.find("def Xform \"Model\"").expect("Model node");
        let rotate_at = text.find("xformOp:rotateXYZ").expect("rotation authored");
        assert!(
            model_at < rotate_at,
            "correction must be authored inside the Model node: {text}"
        );
        assert!(text.contains("double3 xformOp:scale = (0.01, 0.01, 0.01)"));
        assert!(text.contains("double3 xformOp:rotateXYZ = (-90, 0, 0)"));
    }

    #[test]
    fn xr_layer_without_controllers_authors_bare_visible_poses() {
        let text = xr_layer_text(None);
        assert!(!text.contains("references"));
        assert!(!text.contains("def Xform \"Model\""));
        assert!(text.contains("def Xform \"Left\""));
        assert!(text.contains("def Xform \"Right\""));
        // No invisible proxy remnant, even without a resolved asset.
        assert!(!text.contains("visibility = \"invisible\""));
        // Bare pose Xforms still exist as per-frame pose-write targets.
        assert!(text.contains("matrix4d xformOp:transform = ( (1, 0, 0, 0)"));
    }

    #[test]
    fn xr_layer_mounts_pointers_scope_with_left_right() {
        // Pointers ride alongside Controllers under /World/Player, and are
        // mounted regardless of whether controller assets resolved.
        for text in [xr_layer_text(None), {
            let model = ControllerModelLayer {
                left: ControllerAsset {
                    asset_path: "/assets/left.usdc".into(),
                    settings: CoordinateSettings::WORLD,
                    mount_offset: None,
                },
                right: ControllerAsset {
                    asset_path: "/assets/right.usdc".into(),
                    settings: CoordinateSettings::WORLD,
                    mount_offset: None,
                },
            };
            xr_layer_text(Some(&model))
        }] {
            assert!(text.contains("def Xform \"Pointers\""), "{text}");
            // Both scopes must be present as siblings under /World/Player.
            let ctrl_at = text.find("def Xform \"Controllers\"").expect("Controllers");
            let ptr_at = text.find("def Xform \"Pointers\"").expect("Pointers");
            assert!(ctrl_at < ptr_at, "Controllers should precede Pointers");
        }
    }

    #[test]
    fn mount_offset_from_config_maps_trs_with_unit_scale_default() {
        // Absent translate/rotate → zero; absent scale → unit.
        let cfg = MountConfig {
            translate: None,
            rotate: Some([180.0, 0.0, 0.0]),
            scale: None,
        };
        let m = mount_offset_from_config(cfg);
        assert_eq!(m.rotate_xyz, [180.0, 0.0, 0.0]);
        assert_eq!(m.translate, [0.0, 0.0, 0.0]);
        assert_eq!(m.scale, [1.0, 1.0, 1.0], "absent scale reads as unit");
    }

    #[test]
    fn mount_offset_emits_mount_node_in_xr_layer() {
        let model = ControllerModelLayer {
            left: ControllerAsset {
                asset_path: "/assets/qp.usdc".into(),
                settings: CoordinateSettings::from_usd("Z", 0.01),
                mount_offset: Some(MountOffset {
                    rotate_xyz: [180.0, 0.0, 0.0],
                    translate: [0.0, 0.0, 0.0],
                    scale: [1.0, 1.0, 1.0],
                }),
            },
            right: ControllerAsset {
                asset_path: "/assets/qp.usdc".into(),
                settings: CoordinateSettings::from_usd("Z", 0.01),
                mount_offset: None,
            },
        };
        let text = xr_layer_text(Some(&model));
        // The offset is authored on a Mount node wrapping Model, outside the
        // normalization ops (which stay on Model).
        let mount_at = text.find("def Xform \"Mount\"").expect("Mount node");
        let model_at = text.find("def Xform \"Model\"").expect("Model node");
        assert!(mount_at < model_at, "Mount must wrap Model");
        // The seeded rotate (from run.toml TRS) is authored on the Mount node.
        assert!(
            text.contains("xformOp:rotateXYZ"),
            "Mount carries the rotate op"
        );
    }

    #[test]
    fn asset_path_uses_forward_slashes() {
        let p = Path::new(r"C:\a\b\world.usda");
        assert_eq!(usd_asset_path(p), "C:/a/b/world.usda");
    }

    /// Repo `tests/fixtures/usd` dir, relative to this crate.
    fn fixtures_usd() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/usd")
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("idtx-asm-it-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Project-preview composition shape: the sublayer stack
    /// `[player_overlay (strong), level (weak)]` must compose the level's world
    /// content with **exactly one** `/World/Player` from the mode-selected player
    /// overlay — the regression guard against the double-player bug (the open
    /// tab's baked assembly pinned one player *and* the runtime injected another).
    #[test]
    fn project_preview_composes_single_player_over_level() {
        let usd = fixtures_usd();
        let level = usd.join("hydration/preview_world.usda");
        assert!(level.is_file(), "missing fixture {}", level.display());

        let dir = scratch_dir("project-preview");

        // A minimal mode-selected player overlay: `over "World"` with a single
        // `def Xform "Player"` (as the real desktop/xr player stages author).
        let player = dir.join("player.usda");
        std::fs::write(
            &player,
            "#usda 1.0\n(\n    doc = \"test player overlay\"\n)\n\n\
             over \"World\"\n{\n    def Xform \"Player\"\n    {\n        \
             def Camera \"Camera\"\n        {\n        }\n    }\n}\n",
        )
        .unwrap();

        // The project path authors `[player (strong), level (weak)]`.
        let assembly = dir.join("assembly-root.usda");
        std::fs::write(
            &assembly,
            assembly_layer_text(
                Some("World"),
                "Y",
                0.01,
                ShadowMode::Off,
                &[usd_asset_path(&player), usd_asset_path(&level)],
            ),
        )
        .unwrap();

        let mut stage = StageService::open_topology_only(&assembly).expect("open assembly");
        let names: Vec<String> = stage
            .project_topology()
            .expect("project topology")
            .iter()
            .map(|r| r.name.clone())
            .collect();

        // The level's world content composes …
        assert!(names.contains(&"World".to_string()), "{names:?}");
        assert!(
            names.contains(&"Ball".to_string()),
            "level content: {names:?}"
        );
        // … and exactly one Player rides above it (no double-player).
        assert!(names.contains(&"Player".to_string()), "{names:?}");
        assert_eq!(
            names.iter().filter(|n| *n == "Player").count(),
            1,
            "exactly one player must compose over the level: {names:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Composition with a Z-up / centimetre controller asset: the generated XR
    /// `/World/Player/Controllers/Left/Model` carrying the metre / Y-up
    /// correction ops, and the composed stage must still expose the proxy subtree.
    #[test]
    fn xr_assembly_composes_zup_controller_with_correction() {
        let usd = fixtures_usd();
        let world = usd.join("hydration/preview_world.usda");
        let controller = usd.join("controllers/zup_cm.usda");
        assert!(world.is_file(), "missing fixture {}", world.display());
        assert!(
            controller.is_file(),
            "missing fixture {}",
            controller.display()
        );

        // The correction is derived from the asset's own metadata (Z-up, cm).
        let settings = read_controller_settings(&controller);
        let ops = idtx_studio_session::xr_controller_prefab::normalization_ops(
            settings,
            CoordinateSettings::WORLD,
        )
        .expect("zup_cm.usda must yield a metre/Y-up correction");
        let joined = ops.join("\n");
        assert!(
            joined.contains("double3 xformOp:rotateXYZ = (-90, 0, 0)"),
            "{joined}"
        );
        assert!(
            joined.contains("double3 xformOp:scale = (0.01, 0.01, 0.01)"),
            "{joined}"
        );

        let dir = scratch_dir("zup");
        let model = ControllerModelLayer {
            left: ControllerAsset {
                asset_path: usd_asset_path(&controller),
                settings,
                mount_offset: None,
            },
            right: ControllerAsset {
                asset_path: usd_asset_path(&controller),
                settings,
                mount_offset: None,
            },
        };
        let xr = dir.join("controllers-default.usda");
        let xr_text = xr_layer_text(Some(&model));
        // The correction must be authored on the Model node, not the pose root.
        assert!(xr_text.contains("def Xform \"Model\""), "{xr_text}");
        assert!(xr_text.contains("xformOp:rotateXYZ"), "{xr_text}");
        std::fs::write(&xr, &xr_text).unwrap();

        let xr_assembly = dir.join("assembly-xr.usda");
        std::fs::write(
            &xr_assembly,
            assembly_layer_text(
                Some("World"),
                "Y",
                0.01,
                ShadowMode::Off,
                &[usd_asset_path(&xr), usd_asset_path(&world)],
            ),
        )
        .unwrap();

        let mut stage = StageService::open_topology_only(&xr_assembly).expect("open xr assembly");
        let names: Vec<String> = stage
            .project_topology()
            .expect("project topology")
            .iter()
            .map(|r| r.name.clone())
            .collect();
        for expected in ["Player", "Controllers", "Left", "Right", "Model"] {
            assert!(
                names.contains(&expected.to_string()),
                "Z-up XR assembly missing {expected}: {names:?}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
