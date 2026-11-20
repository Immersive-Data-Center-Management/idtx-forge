//! Assembly-stage composition — the single source of truth.
//!
//! This module composes a project's registered levels as **unloaded payload
//! references** under `/World/Levels/<Name>` plus the persistent player/HUD
//! **sublayer**, per roadmap Appendix A.2/A.3. The file is tagged
//! `customLayerData.idtx.stageKind = "assembly"`.
//!
//! It is the one place this logic lives: the editor preview
//! (`idtx-studio-shell`'s `preview_assembly`) re-exports it, and the cook
//! (`crate::cook::author_cook_assembly`) drives it to generate the entry stage
//! it flattens. The whole module is gated behind the `cooking` feature (which
//! enables the `idtx-usd` authoring primitives it needs); a plain `idtx-build`
//! never compiles it.
//!
//! The design splits a **pure planner** ([`plan_assembly`] / [`plan_cook_assembly`],
//! unit-testable without USD) from a thin **executor** ([`author_assembly_stage`],
//! exercised by an integration test). The planner derives a stable, unique USD
//! prim name per level and computes every asset path **relative to the assembly
//! stage's own directory**, so the saved artifact stays portable regardless of
//! where it is written.
//!
//! ## Physics scene composition
//!
//! A level authors its `UsdPhysicsScene` as a **root prim** — a sibling of the
//! default prim, directly under the pseudo-root. The level is composed here as a
//! *default-prim payload* (`/World/Levels/<name>`), which pulls in only the
//! default prim's subtree, so that root-level scene is dropped and the runtime
//! physics subsystem never activates. The executor therefore detects a
//! root-level scene in each *visible* level and authors a typeless root prim that
//! references it explicitly (`def "physicsScene" ( prepend references =
//! @level@</physicsScene> ) {}`). The reference is always composed, so the scene
//! (and its gravity/params) is present even before the level payload loads; the
//! colliders still arrive with the payload. See [`detect_root_physics_scene`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use idtx_usd::ShadowMode;

pub use crate::cook::PlayerRef;

/// One level's holder-prim placement in the authored assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssemblyLevel {
    /// Sanitized, unique USD prim name authored as `/World/Levels/<name>`. This
    /// is the stable identity the (future) transition manager keys on — treat it
    /// as API, not a display string (roadmap A.4).
    pub holder_name: String,
    /// Asset path authored on the payload arc, **relative to the assembly
    /// stage's directory** with forward slashes (e.g. `levels/hub.usda` or
    /// `../levels/hub.usda`), so the composed stage resolves the level wherever
    /// the assembly is saved inside the project.
    pub payload_path: String,
    /// Whether this level starts **visible** in the composed stage. The executor
    /// authors `visibility = "invisible"` on every level for which this is
    /// `false`; a visible level authors no explicit visibility (inherited).
    pub visible: bool,
}

/// How a project's player stage is named when composed into the assembly.
///
/// See [`PlayerRef`] (defined in [`crate::cook`] so it is available even when
/// the `cooking` feature — and this module — is not compiled). The assembly
/// composes the player as a **reference** at `/World/Player` (never a sublayer):
/// a standalone player stage roots its rig at `def Xform "Player"`, and the
/// assembly references that `</Player>` into `/World/Player`.
///
/// A resolved plan for authoring an assembly stage: the ordered level holders
/// plus the optional player reference (asset-path arc, relative to the assembly
/// stage's directory, or a verbatim `built-in/...` arc).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AssemblyPlan {
    /// Level holders in registry order.
    pub levels: Vec<AssemblyLevel>,
    /// The player stage composed as a **reference** at `/World/Player` — the
    /// asset-path arc (assembly-relative for a project stage, verbatim for the
    /// built-in). `None` composes no player.
    pub player_reference: Option<String>,
    /// The `playerKind` variant to select on `/World/Player` (`"desktop"` /
    /// `"xr"`), authored after the reference. `None` leaves the player stage's
    /// own default selection in place. Set by the cook/preview from the target
    /// mode until the runtime chooses the rig from HMD presence (a later
    /// iteration). Ignored when [`player_reference`](Self::player_reference) is
    /// `None`.
    pub player_variant: Option<String>,
}

/// The asset-path arc for the player reference, relativized against
/// `assembly_dir` for a project stage or passed verbatim for the built-in.
fn player_arc(assembly_dir: &Path, player: Option<&PlayerRef>) -> Option<String> {
    player.map(|p| match p {
        PlayerRef::Project(abs) => relative_asset_path(assembly_dir, abs),
        PlayerRef::Builtin(arc) => arc.clone(),
    })
}

/// Stage-level metadata to stamp onto the composed assembly: up-axis, unit
/// scale, and the soft-shadow mode read from the (default) level.
pub struct AssemblyCfg {
    pub up_axis: String,
    pub meters_per_unit: f64,
    pub shadow_mode: ShadowMode,
}

/// Build the [`AssemblyPlan`] for an assembly saved at `assembly_dir`, composing
/// the given absolute level paths (in registry order) and an optional player
/// reference. **Preview rule:** a single level is visible; two or more
/// levels all start invisible (byte-identical to the historical preview output,
/// where the transition manager toggles visibility at runtime).
///
/// Every level path is relativized against `assembly_dir` so the authored arcs
/// are portable. Holder names are derived from each level's file stem, sanitized
/// to a valid USD identifier, and de-duplicated (`Hub`, `Hub_2`, …) so two
/// levels with the same stem still get distinct holder prims.
#[must_use]
pub fn plan_assembly(
    assembly_dir: &Path,
    level_abs_paths: &[PathBuf],
    player: Option<&PlayerRef>,
) -> AssemblyPlan {
    // Preview semantics: the lone level is visible; with >1 level every level
    // starts invisible.
    let single = level_abs_paths.len() == 1;
    let mut used = HashSet::new();
    let levels = level_abs_paths
        .iter()
        .map(|abs| AssemblyLevel {
            holder_name: unique_holder_name(&stem_of(abs), &mut used),
            payload_path: relative_asset_path(assembly_dir, abs),
            visible: single,
        })
        .collect();
    AssemblyPlan {
        levels,
        player_reference: player_arc(assembly_dir, player),
        player_variant: None,
    }
}

/// Build the [`AssemblyPlan`] for a **cook** assembly: exactly one level starts
/// visible — the configured `default_level` — and every other level starts
/// invisible. When `default_level` is `None` (or does not match any registered
/// level), the first level is the visible one. This is the packaged-experience
/// twin of [`plan_assembly`]; it differs only in the visibility rule.
#[must_use]
pub fn plan_cook_assembly(
    assembly_dir: &Path,
    level_abs_paths: &[PathBuf],
    default_level: Option<&Path>,
    player: Option<&PlayerRef>,
) -> AssemblyPlan {
    // The visible index: the default level's position, else the first level.
    let default_idx =
        default_level.and_then(|def| level_abs_paths.iter().position(|p| paths_equal(p, def)));
    let visible_idx = default_idx.or_else(|| (!level_abs_paths.is_empty()).then_some(0));

    let mut used = HashSet::new();
    let levels = level_abs_paths
        .iter()
        .enumerate()
        .map(|(i, abs)| AssemblyLevel {
            holder_name: unique_holder_name(&stem_of(abs), &mut used),
            payload_path: relative_asset_path(assembly_dir, abs),
            visible: Some(i) == visible_idx,
        })
        .collect();
    AssemblyPlan {
        levels,
        player_reference: player_arc(assembly_dir, player),
        player_variant: None,
    }
}

/// The file stem of a path as an owned string (empty when there is none).
pub(crate) fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Whether two absolute paths denote the same file, tolerating a Windows
/// verbatim (`\\?\`) prefix on either side (both `levels_abs()` and
/// `default_stage_abs()` canonicalize, which yields verbatim paths on Windows,
/// but a caller may pass a plain path).
pub(crate) fn paths_equal(a: &Path, b: &Path) -> bool {
    idtx_project::strip_verbatim_prefix(a) == idtx_project::strip_verbatim_prefix(b)
}

/// Sanitize an arbitrary string into a valid USD prim identifier: the first
/// character must be a letter or `_`; subsequent characters may be alphanumeric
/// or `_`; every other character is replaced by `_`. An empty or fully-invalid
/// input yields `"Level"`.
#[must_use]
fn sanitize_prim_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for (i, ch) in raw.chars().enumerate() {
        let ok = if i == 0 {
            ch.is_ascii_alphabetic() || ch == '_'
        } else {
            ch.is_ascii_alphanumeric() || ch == '_'
        };
        out.push(if ok { ch } else { '_' });
    }
    // A leading digit (valid alphanumeric but invalid first char) collapses to
    // `_`; strip a degenerate all-underscore/empty result to the fallback.
    if out.is_empty() || out.chars().all(|c| c == '_') {
        return "Level".to_owned();
    }
    out
}

/// Sanitize `stem` and make it unique against `used`, appending `_2`, `_3`, …
/// on collision. Records the chosen name in `used`.
pub(crate) fn unique_holder_name(stem: &str, used: &mut HashSet<String>) -> String {
    let base = sanitize_prim_name(stem);
    if used.insert(base.clone()) {
        return base;
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{base}_{n}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// Compute a forward-slash asset path for `target` relative to `from_dir`,
/// walking up with `..` where needed. Falls back to `target`'s lossy string
/// (forward-slashed) when the two share no common prefix (e.g. different
/// Windows drives) — a saved assembly is always inside the project, so the
/// fallback is a defensive last resort, not the normal path.
#[must_use]
pub(crate) fn relative_asset_path(from_dir: &Path, target: &Path) -> String {
    use std::path::Component;

    // Normalize away Windows verbatim (`\\?\`) prefixes on both sides first.
    // `std::fs::canonicalize` (used to resolve level/player paths) returns
    // verbatim paths on Windows, whose `Prefix(VerbatimDisk('C'))` component
    // never matches a temp dir's plain `Prefix(Disk('C'))`. Without this the
    // shared-prefix count is 0 and the fallback below emits a raw `//?/C:/…`
    // string — which USD accepts as a direct layer identifier but cannot anchor
    // a relative reference against, silently dropping the referenced payload.
    let from_dir = idtx_project::strip_verbatim_prefix(from_dir);
    let target = idtx_project::strip_verbatim_prefix(target);

    let base: Vec<Component<'_>> = from_dir
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    let dest: Vec<Component<'_>> = target
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();

    // Length of the shared prefix.
    let common = base
        .iter()
        .zip(dest.iter())
        .take_while(|(a, b)| a == b)
        .count();

    // A path is only relativizable when it shares the filesystem prefix
    // (root / drive component) with the base.
    if common == 0 {
        return target.to_string_lossy().replace('\\', "/");
    }

    let ups = base.len() - common;
    let mut parts: Vec<String> = std::iter::repeat_n("..".to_owned(), ups).collect();
    for comp in &dest[common..] {
        parts.push(comp.as_os_str().to_string_lossy().into_owned());
    }
    parts.join("/")
}

/// Detect a **root-level** `UsdPhysicsScene` prim in `level_path` — a sibling of
/// the level's default prim, authored directly under the pseudo-root. Returns the
/// scene prim's absolute USD path (e.g. `/physicsScene`), or `None` when the
/// level has no root-level scene, cannot be opened, or its scene is nested inside
/// the default prim (already pulled in by the level's default-prim payload, so
/// re-referencing it would duplicate the scene).
///
/// Only the authored prim *type token* is read, so detection does not require the
/// linked `usdPhysics` plugin. Any open/projection failure resolves to `None` —
/// physics composition is best-effort ("if available").
#[must_use]
fn detect_root_physics_scene(level_path: &Path) -> Option<String> {
    use idtx_usd::StageService;

    let mut stage = StageService::open_topology_only(level_path).ok()?;
    // Clone the projection so the `&mut` borrow is released before `path_for`.
    let records = stage.project_topology().ok()?.to_vec();
    let pseudo_root = records.iter().find(|r| r.is_pseudo_root).map(|r| r.prim);
    let scene = records.iter().find(|r| {
        r.type_name == idtx_usd_extension::PHYSICS_SCENE_TYPE
            && !r.is_pseudo_root
            && r.parent == pseudo_root
    })?;
    stage.path_for(scene.prim).map(str::to_owned)
}

/// Detect a level's **stage-level behavior** prim and return its absolute USD
/// path, or `None` when the level assigns no stage behavior, cannot be opened,
/// or the named prim is not a root-level prim.
///
/// The stage behavior lives at `/StageBehavior` (by convention) — a `def Scope`
/// carrying `IdtxBehaviorAPI`, a *sibling* of the level's default prim. Exactly
/// like the root `UsdPhysicsScene` (see [`detect_root_physics_scene`]), the
/// default-prim payload the assembly authors for the level pulls in only the
/// default-prim subtree, so this sibling is **dropped**. Without re-surfacing it
/// in the assembly root the behavior-prepare pass never sees its
/// `idtx:behavior:source` (so the module is never compiled or linked) and the
/// runtime never hydrates it.
///
/// The prim name is read authoritatively from the root layer's
/// `customLayerData["idtx:stageBehavior"]` ([`idtx_usd::StageMetadata::
/// stage_behavior_name`]); any open/projection failure resolves to `None`
/// (best-effort, matching the physics-scene probe).
#[must_use]
fn detect_root_stage_behavior(level_path: &Path) -> Option<String> {
    use idtx_usd::StageService;

    let mut stage = StageService::open_topology_only(level_path).ok()?;
    let name = stage.metadata().stage_behavior_name.clone()?;
    if name.is_empty() {
        return None;
    }
    let target = format!("/{name}");
    // Clone the projection so the `&mut` borrow is released before `path_for`.
    let records = stage.project_topology().ok()?.to_vec();
    let pseudo_root = records.iter().find(|r| r.is_pseudo_root).map(|r| r.prim);
    // Confirm the named prim exists as a root-level prim (sibling of the default
    // prim) — only such a prim is dropped by the default-prim payload.
    records
        .iter()
        .filter(|r| !r.is_pseudo_root && r.parent == pseudo_root)
        .find(|r| stage.path_for(r.prim) == Some(target.as_str()))
        .map(|_| target)
}

/// Author (and save) a persistent assembly stage at `save_path` from `plan`.
///
/// Creates the stage (`/World` Xform root), tags it `stageKind = "assembly"`,
/// defines the `/World/Levels` holder scope, attaches each level as an
/// **unloaded payload reference** under its holder prim, authors
/// `visibility = "invisible"` on every level that is **not** marked
/// [`AssemblyLevel::visible`], composes each visible level's root-level
/// `UsdPhysicsScene` (see below), composes the player stage as a reference at
/// `/World/Player` if present, and saves to disk.
///
/// A level's `UsdPhysicsScene` is a sibling of its default prim, so the
/// default-prim payload above does not pull it in; without re-referencing it the
/// runtime physics subsystem never finds a scene and simulation stays off. For
/// each **visible** level (at most one in every planner path) carrying a
/// root-level scene, a typeless `def "<scene>" ( prepend references =
/// @level@</scene> ) {}` prim is authored at the assembly root — the full-stage
/// topology traversal then hydrates it. See [`detect_root_physics_scene`].
///
/// The player stage is a standalone stage rooted at `def Xform "Player"`; the
/// assembly composes it by authoring `def Xform "Player"` under `/World` with a
/// reference to the stage's `</Player>` prim. The rig, its `playerKind`
/// variantSet, and its behaviors land at `/World/Player/...` — where the
/// renderer, XR, and behavior path constants expect them — and the referenced
/// `def`s survive the `IsDefined` topology-traversal predicate. No `/World/XR`
/// bootstrap scope is authored.
///
/// # Errors
///
/// Propagates any [`idtx_usd::StageError`] from stage creation, structural
/// authoring, or the final save.
pub fn author_assembly_stage(
    save_path: &Path,
    plan: &AssemblyPlan,
    config: Option<&AssemblyCfg>,
    display_name: &str,
) -> Result<(), idtx_usd::StageError> {
    use idtx_usd::{PropertyValue, StageService, StructuralCommand};

    let define = |path: &str, type_name: &str| StructuralCommand::Define {
        path: path.to_owned(),
        type_name: type_name.to_owned(),
    };

    let mut stage = StageService::create_new(
        save_path,
        config.map_or("Y", |c| &c.up_axis),
        config.map_or(1.0, |c| c.meters_per_unit),
    )?;
    stage.set_stage_kind("assembly", display_name)?;
    if let Some(token) = config.map(|c| c.shadow_mode) {
        stage.author_root_shadow_mode(token)?;
    }

    // Holder scope for the level payloads. (No `/World/XR` bootstrap: the
    // player is composed as a `def Xform "Player"` reference under `/World`
    // below, so its controllers already nest under a `def` `/World/Player`.)
    stage.apply_structural_command(&define("/World/Levels", "Scope"))?;

    for level in &plan.levels {
        let holder = format!("/World/Levels/{}", level.holder_name);
        // Typeless `def "<name>"` — a bare payload holder, per the A.2 layout.
        stage.apply_structural_command(&define(&holder, ""))?;
        stage.project_topology()?;
        let prim =
            stage
                .prim_at_path(&holder)
                .ok_or_else(|| idtx_usd::StageError::InvalidProjection {
                    path: holder.clone(),
                    detail: "level holder prim did not project after Define".to_owned(),
                })?;
        stage.apply_structural_command(&StructuralCommand::AddPayload {
            prim,
            asset_path: level.payload_path.clone(),
            target_path: None,
        })?;

        // Hide every level that is not the start-visible one; the transition
        // manager toggles visibility at runtime for the rest.
        if !level.visible {
            let prim = stage.prim_at_path(&holder).ok_or_else(|| {
                idtx_usd::StageError::InvalidProjection {
                    path: holder.clone(),
                    detail: "level holder prim vanished before visibility write".to_owned(),
                }
            })?;
            stage.apply_structural_command(&StructuralCommand::SetProperty {
                prim,
                property: "visibility".to_owned(),
                value: PropertyValue::Token("invisible".to_owned()),
            })?;
        }
    }

    // Compose each visible level's root-level `UsdPhysicsScene` (dropped by the
    // default-prim payload above). Only visible levels contribute — at most one
    // in every planner path — so a single scene composes, matching the runtime
    // subsystem's single-scene model (it simulates the first and warns on extras).
    let assembly_dir = save_path.parent().unwrap_or_else(|| Path::new("."));
    let mut used_scene_names: HashSet<String> = HashSet::new();
    for level in plan.levels.iter().filter(|l| l.visible) {
        // Payload paths are authored relative to the assembly stage's own dir.
        let level_abs = assembly_dir.join(&level.payload_path);
        let Some(scene_path) = detect_root_physics_scene(&level_abs) else {
            continue;
        };
        let leaf = scene_path.rsplit('/').next().unwrap_or("physicsScene");
        let name = unique_holder_name(leaf, &mut used_scene_names);
        let holder = format!("/{name}");
        // Typeless `def "<name>"` — the scene type comes from the reference.
        stage.apply_structural_command(&define(&holder, ""))?;
        stage.project_topology()?;
        let prim =
            stage
                .prim_at_path(&holder)
                .ok_or_else(|| idtx_usd::StageError::InvalidProjection {
                    path: holder.clone(),
                    detail: "physics-scene holder prim did not project after Define".to_owned(),
                })?;
        stage.apply_structural_command(&StructuralCommand::AddReference {
            prim,
            asset_path: level.payload_path.clone(),
            target_path: Some(scene_path),
        })?;
    }

    // Surface each level's root-level **stage behavior** (the `/StageBehavior`
    // sibling of the default prim, dropped by the default-prim payload above) as
    // a nested **payload** under that level's own holder, at
    // `/World/Levels/<name>/StageBehavior`. A payload arc (not a reference) keeps
    // the compose context classified as `Payload` — matching how the level itself
    // is composed — so `stage_behavior_compose_context` stays consistent (a
    // reference arc would classify as `Reference`, which never runs). Nesting per
    // level means multiple levels never collide at a single root path and the
    // behavior loads/unloads with its owning level. Without this the
    // behavior-prepare pass never sees the behavior's `idtx:behavior:source` (so
    // its module is neither compiled nor linked) and the runtime never hydrates
    // it. The level's `idtx:stageBehaviorRunScope` still gates whether it ticks.
    for level in &plan.levels {
        let level_abs = assembly_dir.join(&level.payload_path);
        let Some(sb_path) = detect_root_stage_behavior(&level_abs) else {
            continue;
        };
        let leaf = sb_path.trim_start_matches('/');
        let holder = format!("/World/Levels/{}/{leaf}", level.holder_name);
        // Typeless `def "<leaf>"` — the type (`Scope`) + `IdtxBehaviorAPI` come
        // from the payloaded level prim.
        stage.apply_structural_command(&define(&holder, ""))?;
        stage.project_topology()?;
        let prim = stage.prim_at_path(&holder).ok_or_else(|| {
            idtx_usd::StageError::InvalidProjection {
                path: holder.clone(),
                detail: "stage-behavior holder prim did not project after Define".to_owned(),
            }
        })?;
        stage.apply_structural_command(&StructuralCommand::AddPayload {
            prim,
            asset_path: level.payload_path.clone(),
            target_path: Some(sb_path.clone()),
        })?;
    }

    // Compose the player as a **reference** at `/World/Player`: a standalone
    // player stage roots its rig at `def Xform "Player"`, and we reference that
    // `</Player>` into `/World/Player` so the rig, its variantSet, and its
    // behaviors land exactly where the renderer / XR / behavior path constants
    // expect. The referenced prims are `def`s, so they survive the `IsDefined`
    // topology-traversal predicate.
    if let Some(player) = &plan.player_reference {
        stage.apply_structural_command(&define("/World/Player", "Xform"))?;
        stage.project_topology()?;
        let prim = stage.prim_at_path("/World/Player").ok_or_else(|| {
            idtx_usd::StageError::InvalidProjection {
                path: "/World/Player".to_owned(),
                detail: "player holder prim did not project after Define".to_owned(),
            }
        })?;
        stage.apply_structural_command(&StructuralCommand::AddReference {
            prim,
            asset_path: player.clone(),
            target_path: Some("/Player".to_owned()),
        })?;

        // Re-stamp the `idtx:playerRole = "local"` authority seam on the composed
        // pawn root. The referenced built-in/project player *asset* is role-neutral
        // (it is shared by every peer and must not bake "local"); the composing
        // overlay is what names *this* pawn the local player — exactly as the
        // retired per-project `xr_player.usda` / `desktop_player.usda` overlays did
        // before the player-stage refactor. Without it nothing hydrates a
        // `PlayerRole::Local`, so `local_player_root_world` (the XR eye
        // `stage_to_world`) and `local_player_camera` (the desktop runtime camera)
        // both resolve `None`: the capsule + controllers still follow the pawn via
        // USD composition, but the rendered viewpoint stops tracking locomotion /
        // snap-turn. Author on a freshly projected handle (the reference above
        // changed the composition).
        stage.project_topology()?;
        let prim = stage.prim_at_path("/World/Player").ok_or_else(|| {
            idtx_usd::StageError::InvalidProjection {
                path: "/World/Player".to_owned(),
                detail: "player prim unavailable before role authoring".to_owned(),
            }
        })?;
        stage.apply_structural_command(&StructuralCommand::SetProperty {
            prim,
            property: idtx_usd::PLAYER_ROLE_ATTR.to_owned(),
            value: PropertyValue::Token("local".to_owned()),
        })?;

        // Select the `playerKind` variant for the target mode (desktop / xr).
        // The cook/preview bakes the mode's selection. Leaving `player_variant`
        // as `None` keeps the player stage's own authored default (`desktop`).
        if let Some(variant) = &plan.player_variant {
            stage.project_topology()?;
            let prim = stage.prim_at_path("/World/Player").ok_or_else(|| {
                idtx_usd::StageError::InvalidProjection {
                    path: "/World/Player".to_owned(),
                    detail: "player prim unavailable before variant selection".to_owned(),
                }
            })?;
            stage.set_variant_layer_state(
                prim,
                "playerKind",
                &idtx_usd::VariantLayerState {
                    root_selection: Some(variant.clone()),
                    session_selection: None,
                },
            )?;
        }
    }

    stage.save_stage(save_path)?;
    Ok(())
}

/// The fixed holder prim name a per-level assembly re-roots its level body under
/// (`/World/level`). Stable identity the runtime/driver keys on — treat as API.
pub const PER_LEVEL_HOLDER: &str = "level";

/// A resolved plan for authoring a **per-level assembly** (the Scenario-2 level
/// composition): one level body composed over the shared player-rig and shared
/// global sublayers. All arcs are **relative to the assembly stage's own
/// directory** (or a verbatim `built-in/...` arc for the player), so the saved
/// `level.usda` is portable within the cooked tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerLevelAssembly {
    /// Asset-path arc to the level **body** (`.usdz`), relative to the assembly
    /// dir (e.g. `"forest.usdz"`). Composed as a **reference** under
    /// `/World/level` — Scenario-2 never load/unloads, so the body loads eagerly
    /// when the assembly session opens.
    pub body_arc: String,
    /// Player stage arc (assembly-relative for a project stage, verbatim for the
    /// built-in), composed as a reference at `/World/Player`. `None` composes no
    /// player.
    pub player_reference: Option<String>,
    /// The `playerKind` variant to select on `/World/Player` (`"desktop"`/`"xr"`).
    /// Ignored when [`player_reference`](Self::player_reference) is `None`.
    pub player_variant: Option<String>,
    /// Shared global sublayer arcs (assembly-relative), authored as root
    /// `subLayer`s in listed order — prototypes/definitions shared by every level.
    pub shared_sublayers: Vec<String>,
}

/// Author (and save) a **per-level assembly** stage at `save_path` from `plan`.
///
/// Composes a single level body over the shared player-rig and shared global
/// sublayers (the Scenario-2 level-transition composition — see
/// `docs/level-transition-runtime-plan.md` §3):
///
/// ```text
/// (subLayers = [<shared_sublayers…>])                       # prototypes/defs for every level
/// def Xform "World" {
///     def "level" ( references = @body@ ) {                 # body re-rooted under /World/level
///         def "StageBehavior" ( payload = @body@</StageBehavior> ) {}   # re-surfaced, PAYLOAD so it runs
///     }
///     def Xform "Player" ( references = @player@</Player> ; variants = {playerKind} )
///         { token idtx:playerRole = "local" }
/// }
/// def "physicsScene" ( references = @body@</physicsScene> ) {}          # re-surfaced root sibling
/// ```
///
/// The body is a **reference** (not a payload): Scenario-2 opens the whole
/// assembly session async and swaps it wholesale, so there is no payload
/// load/unload to toggle. Because the body composes via its **default prim**, its
/// root-level siblings (`/StageBehavior`, `/physicsScene`) are dropped and
/// re-surfaced here — the `StageBehavior` as a **payload** so its compose context
/// classifies as runnable (independent of the body's reference arc); the physics
/// scene as a reference (detected by prim type, not compose context). Both are
/// best-effort: a body with neither re-surfaces nothing.
///
/// `probe_path` is the **absolute** path of a stage with the body's root-level
/// prim structure (the cooked body, or the source level) used only to detect the
/// root `StageBehavior`/`physicsScene`; the authored arcs always use
/// [`PerLevelAssembly::body_arc`].
///
/// # Errors
///
/// Propagates any [`idtx_usd::StageError`] from stage creation, structural
/// authoring, or the final save.
pub fn author_per_level_assembly(
    save_path: &Path,
    plan: &PerLevelAssembly,
    probe_path: &Path,
    config: Option<&AssemblyCfg>,
    display_name: &str,
) -> Result<(), idtx_usd::StageError> {
    use idtx_usd::{PropertyValue, StageService, StructuralCommand};

    let define = |path: &str, type_name: &str| StructuralCommand::Define {
        path: path.to_owned(),
        type_name: type_name.to_owned(),
    };

    let mut stage = StageService::create_new(
        save_path,
        config.map_or("Y", |c| &c.up_axis),
        config.map_or(1.0, |c| c.meters_per_unit),
    )?;

    // Shared global sublayers first — prototypes/definitions that compose into
    // every level's namespace without copying. Authored in listed order.
    for arc in &plan.shared_sublayers {
        stage.apply_structural_command(&StructuralCommand::AddSublayer {
            asset_path: arc.clone(),
        })?;
    }

    stage.set_stage_kind("assembly", display_name)?;
    if let Some(token) = config.map(|c| c.shadow_mode) {
        stage.author_root_shadow_mode(token)?;
    }

    // The level body, re-rooted under `/World/level` as a REFERENCE (no "Levels"
    // scope — a per-level assembly only ever holds one level).
    let holder = format!("/World/{PER_LEVEL_HOLDER}");
    stage.apply_structural_command(&define(&holder, ""))?;
    stage.project_topology()?;
    let prim = stage
        .prim_at_path(&holder)
        .ok_or_else(|| idtx_usd::StageError::InvalidProjection {
            path: holder.clone(),
            detail: "level holder prim did not project after Define".to_owned(),
        })?;
    stage.apply_structural_command(&StructuralCommand::AddReference {
        prim,
        asset_path: plan.body_arc.clone(),
        target_path: None,
    })?;

    // Re-surface the body's root-level stage behavior (dropped by the default-prim
    // reference above) as a nested **payload** under the holder, so its
    // `stage_behavior_compose_context` classifies as Payload (runnable) regardless
    // of the body being referenced. Best-effort.
    if let Some(sb_path) = detect_root_stage_behavior(probe_path) {
        let leaf = sb_path.trim_start_matches('/');
        let sb_holder = format!("{holder}/{leaf}");
        stage.apply_structural_command(&define(&sb_holder, ""))?;
        stage.project_topology()?;
        let prim = stage.prim_at_path(&sb_holder).ok_or_else(|| {
            idtx_usd::StageError::InvalidProjection {
                path: sb_holder.clone(),
                detail: "stage-behavior holder prim did not project after Define".to_owned(),
            }
        })?;
        stage.apply_structural_command(&StructuralCommand::AddPayload {
            prim,
            asset_path: plan.body_arc.clone(),
            target_path: Some(sb_path),
        })?;
    }

    // Re-surface the body's root-level physics scene (also dropped by the
    // default-prim reference) as a typeless root prim referencing it, so the
    // runtime physics subsystem finds a scene. Best-effort.
    if let Some(scene_path) = detect_root_physics_scene(probe_path) {
        let leaf = scene_path.rsplit('/').next().unwrap_or("physicsScene");
        let mut used = HashSet::new();
        let name = unique_holder_name(leaf, &mut used);
        let scene_holder = format!("/{name}");
        stage.apply_structural_command(&define(&scene_holder, ""))?;
        stage.project_topology()?;
        let prim = stage.prim_at_path(&scene_holder).ok_or_else(|| {
            idtx_usd::StageError::InvalidProjection {
                path: scene_holder.clone(),
                detail: "physics-scene holder prim did not project after Define".to_owned(),
            }
        })?;
        stage.apply_structural_command(&StructuralCommand::AddReference {
            prim,
            asset_path: plan.body_arc.clone(),
            target_path: Some(scene_path),
        })?;
    }

    // Compose the shared player as a reference at `/World/Player`, re-stamp the
    // `idtx:playerRole = "local"` authority seam on the composed pawn, and select
    // the `playerKind` variant. Mirrors `author_assembly_stage`.
    if let Some(player) = &plan.player_reference {
        stage.apply_structural_command(&define("/World/Player", "Xform"))?;
        stage.project_topology()?;
        let prim = stage.prim_at_path("/World/Player").ok_or_else(|| {
            idtx_usd::StageError::InvalidProjection {
                path: "/World/Player".to_owned(),
                detail: "player holder prim did not project after Define".to_owned(),
            }
        })?;
        stage.apply_structural_command(&StructuralCommand::AddReference {
            prim,
            asset_path: player.clone(),
            target_path: Some("/Player".to_owned()),
        })?;

        stage.project_topology()?;
        let prim = stage.prim_at_path("/World/Player").ok_or_else(|| {
            idtx_usd::StageError::InvalidProjection {
                path: "/World/Player".to_owned(),
                detail: "player prim unavailable before role authoring".to_owned(),
            }
        })?;
        stage.apply_structural_command(&StructuralCommand::SetProperty {
            prim,
            property: idtx_usd::PLAYER_ROLE_ATTR.to_owned(),
            value: PropertyValue::Token("local".to_owned()),
        })?;

        if let Some(variant) = &plan.player_variant {
            stage.project_topology()?;
            let prim = stage.prim_at_path("/World/Player").ok_or_else(|| {
                idtx_usd::StageError::InvalidProjection {
                    path: "/World/Player".to_owned(),
                    detail: "player prim unavailable before variant selection".to_owned(),
                }
            })?;
            stage.set_variant_layer_state(
                prim,
                "playerKind",
                &idtx_usd::VariantLayerState {
                    root_selection: Some(variant.clone()),
                    session_selection: None,
                },
            )?;
        }
    }

    stage.save_stage(save_path)?;
    Ok(())
}

/// The `idtx:shadowMode` token authored into `customLayerData` for a mode, or
/// `None` for [`ShadowMode::Off`]. Test-only guard mirroring the C++ shim's
/// `author_root_shadow_mode` tokens (`hard`/`smooth`/`soft`).
#[cfg(test)]
fn shadow_mode_token(mode: ShadowMode) -> Option<&'static str> {
    match mode {
        ShadowMode::Off => None,
        ShadowMode::Hard => Some("hard"),
        ShadowMode::Smooth => Some("smooth"),
        ShadowMode::Soft => Some("soft"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_prim_name_produces_valid_identifiers() {
        assert_eq!(sanitize_prim_name("Hub"), "Hub");
        assert_eq!(sanitize_prim_name("forest_01"), "forest_01");
        // A leading digit is an invalid first char → replaced.
        assert_eq!(sanitize_prim_name("01intro"), "_1intro");
        // Spaces / punctuation collapse to underscores.
        assert_eq!(sanitize_prim_name("my level!"), "my_level_");
        // Degenerate inputs fall back.
        assert_eq!(sanitize_prim_name(""), "Level");
        assert_eq!(sanitize_prim_name("---"), "Level");
    }

    #[test]
    fn unique_holder_name_disambiguates_collisions() {
        let mut used = HashSet::new();
        assert_eq!(unique_holder_name("hub", &mut used), "hub");
        assert_eq!(unique_holder_name("hub", &mut used), "hub_2");
        assert_eq!(unique_holder_name("hub", &mut used), "hub_3");
        // A different stem is unaffected.
        assert_eq!(unique_holder_name("forest", &mut used), "forest");
    }

    #[test]
    fn relative_asset_path_handles_sibling_and_parent_dirs() {
        // Level in a subdir of the assembly's dir.
        let dir = Path::new("/proj");
        assert_eq!(
            relative_asset_path(dir, Path::new("/proj/levels/hub.usda")),
            "levels/hub.usda"
        );
        // Assembly saved under stages/, level under a sibling levels/ dir.
        let dir = Path::new("/proj/stages");
        assert_eq!(
            relative_asset_path(dir, Path::new("/proj/levels/hub.usda")),
            "../levels/hub.usda"
        );
        // Same directory.
        let dir = Path::new("/proj");
        assert_eq!(
            relative_asset_path(dir, Path::new("/proj/player.usda")),
            "player.usda"
        );
    }

    /// A canonicalized (`\\?\C:\…`) target must relativize against a plain
    /// (`C:\…`) temp dir just like a non-verbatim target — the verbatim prefix is
    /// stripped before component comparison so no `//?/` is ever emitted. This is
    /// the direct regression guard for the missing-payload preview bug.
    #[cfg(windows)]
    #[test]
    fn relative_asset_path_strips_verbatim_prefix_and_relativizes() {
        let dir = Path::new(r"C:\proj\stages");
        // Same target reached via a verbatim and a plain path must agree.
        let via_plain = relative_asset_path(dir, Path::new(r"C:\proj\levels\hub.usda"));
        let via_verbatim = relative_asset_path(dir, Path::new(r"\\?\C:\proj\levels\hub.usda"));
        assert_eq!(via_plain, "../levels/hub.usda");
        assert_eq!(via_verbatim, via_plain);
        assert!(
            !via_verbatim.contains("//?/"),
            "verbatim prefix must not leak into the asset path: {via_verbatim}"
        );
    }

    #[test]
    fn plan_assembly_derives_holders_and_relative_payloads() {
        let dir = Path::new("/proj/stages");
        let levels = vec![
            PathBuf::from("/proj/levels/hub.usda"),
            PathBuf::from("/proj/levels/forest.usda"),
            // Duplicate stem in a different dir → holder must disambiguate.
            PathBuf::from("/proj/extra/hub.usda"),
        ];
        let player = PlayerRef::Project(PathBuf::from("/proj/player.usda"));
        let plan = plan_assembly(dir, &levels, Some(&player));

        assert_eq!(plan.levels.len(), 3);
        assert_eq!(plan.levels[0].holder_name, "hub");
        assert_eq!(plan.levels[0].payload_path, "../levels/hub.usda");
        assert_eq!(plan.levels[1].holder_name, "forest");
        assert_eq!(plan.levels[2].holder_name, "hub_2");
        assert_eq!(plan.levels[2].payload_path, "../extra/hub.usda");
        assert_eq!(plan.player_reference.as_deref(), Some("../player.usda"));
    }

    #[test]
    fn plan_assembly_builtin_player_is_verbatim_arc() {
        // A built-in player is authored as its verbatim search arc — never
        // relativized against the assembly dir (it lives outside the project).
        let dir = Path::new("/proj/stages");
        let player = PlayerRef::Builtin("built-in/players/Player.usd".to_owned());
        let plan = plan_assembly(dir, &[PathBuf::from("/proj/levels/hub.usda")], Some(&player));
        assert_eq!(
            plan.player_reference.as_deref(),
            Some("built-in/players/Player.usd"),
            "built-in arc must pass through verbatim"
        );
    }

    #[test]
    fn plan_assembly_single_level_visible_multi_invisible() {
        // A lone level is visible (preview shows it immediately).
        let plan = plan_assembly(Path::new("/proj"), &[PathBuf::from("/proj/hub.usda")], None);
        assert_eq!(plan.levels.len(), 1);
        assert!(plan.levels[0].visible, "the sole level must be visible");

        // Two or more levels all start invisible (transition manager toggles).
        let plan = plan_assembly(
            Path::new("/proj"),
            &[PathBuf::from("/proj/a.usda"), PathBuf::from("/proj/b.usda")],
            None,
        );
        assert!(
            plan.levels.iter().all(|l| !l.visible),
            "multi-level all hidden"
        );
    }

    #[test]
    fn plan_cook_assembly_marks_only_default_visible() {
        let dir = Path::new("/proj");
        let levels = vec![
            PathBuf::from("/proj/a.usda"),
            PathBuf::from("/proj/b.usda"),
            PathBuf::from("/proj/c.usda"),
        ];
        // The default level is the only visible one; the rest are hidden.
        let plan = plan_cook_assembly(dir, &levels, Some(Path::new("/proj/b.usda")), None);
        let visible: Vec<_> = plan.levels.iter().map(|l| l.visible).collect();
        assert_eq!(visible, vec![false, true, false]);
        assert_eq!(plan.levels.iter().filter(|l| l.visible).count(), 1);

        // A verbatim-prefixed default still matches a plain level path.
        #[cfg(windows)]
        {
            let dir = Path::new(r"C:\proj");
            let levels = vec![
                PathBuf::from(r"C:\proj\a.usda"),
                PathBuf::from(r"C:\proj\b.usda"),
            ];
            let plan =
                plan_cook_assembly(dir, &levels, Some(Path::new(r"\\?\C:\proj\b.usda")), None);
            assert_eq!(
                plan.levels.iter().map(|l| l.visible).collect::<Vec<_>>(),
                vec![false, true]
            );
        }

        // No default (or an unmatched default) → the first level is visible.
        let plan = plan_cook_assembly(dir, &levels, None, None);
        assert!(plan.levels[0].visible);
        assert!(plan.levels[1..].iter().all(|l| !l.visible));

        let plan = plan_cook_assembly(dir, &levels, Some(Path::new("/proj/nope.usda")), None);
        assert!(
            plan.levels[0].visible,
            "unmatched default falls back to first"
        );
    }

    #[test]
    fn plan_assembly_without_player_or_levels_is_empty() {
        let plan = plan_assembly(Path::new("/proj"), &[], None);
        assert!(plan.levels.is_empty());
        assert!(plan.player_reference.is_none());
    }

    /// A verbatim-canonicalized level/player path (as `resolve_relative` yields on
    /// Windows) must produce clean relative arcs with no `//?/` prefix — the
    /// regression guard for the Desktop-preview dropped-payload bug.
    #[cfg(windows)]
    #[test]
    fn plan_assembly_never_emits_verbatim_prefix() {
        let dir = Path::new(r"C:\Users\dev\AppData\Local\Temp\idtx-preview-123");
        let levels = vec![PathBuf::from(r"\\?\C:\proj\stages\level1.usda")];
        let player = PlayerRef::Project(PathBuf::from(r"\\?\C:\proj\stages\desktop_player.usda"));
        let plan = plan_assembly(dir, &levels, Some(&player));

        assert_eq!(plan.levels.len(), 1);
        assert!(
            !plan.levels[0].payload_path.contains("//?/"),
            "payload path must not carry a verbatim prefix: {}",
            plan.levels[0].payload_path
        );
        assert!(
            !plan
                .player_reference
                .as_deref()
                .unwrap_or_default()
                .contains("//?/"),
            "player reference must not carry a verbatim prefix: {:?}",
            plan.player_reference
        );
    }

    /// Unique scratch directory under the OS temp dir.
    fn scratch_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "idtx-asm-author-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write a minimal valid stage file so payload/sublayer targets resolve.
    fn write_stub_stage(path: &Path) {
        std::fs::write(
            path,
            "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\ndef Xform \"World\" {}\n",
        )
        .unwrap();
    }

    /// Write a minimal standalone player stage (root `def Xform "Player"`,
    /// `defaultPrim = "Player"`) so the assembly's `</Player>` reference resolves.
    fn write_player_stub(path: &Path) {
        std::fs::write(
            path,
            "#usda 1.0\n(\n    defaultPrim = \"Player\"\n)\n\
             def Xform \"Player\"\n{\n    \
             def Xform \"Head\"\n    {\n        def Camera \"Camera\"\n        {\n        }\n    }\n}\n",
        )
        .unwrap();
    }

    /// End-to-end authoring: compose two levels (one with a colliding stem) plus
    /// a player reference, save, then assert the on-disk layer carries the
    /// assembly tag, invisible payload holders, and the `/World/Player` reference,
    /// and that `OpenUSD` re-composes the holder prims plus the referenced player.
    /// Links `OpenUSD` (needs `IDTX_USD_DIR`).
    #[test]
    fn author_assembly_stage_composes_invisible_payloads_and_player_reference() {
        use idtx_usd::StageService;

        let dir = scratch_dir("compose");
        let levels_dir = dir.join("levels");
        std::fs::create_dir_all(&levels_dir).unwrap();

        let hub = levels_dir.join("hub.usda");
        let forest = levels_dir.join("forest.usda");
        // A second `hub` stem in a different dir → holder must disambiguate.
        let extra_dir = dir.join("extra");
        std::fs::create_dir_all(&extra_dir).unwrap();
        let hub2 = extra_dir.join("hub.usda");
        let player = dir.join("player.usda");
        for p in [&hub, &forest, &hub2] {
            write_stub_stage(p);
        }
        // The player is a standalone stage rooted at `def Xform "Player"` so the
        // assembly can reference its `</Player>` into `/World/Player`.
        write_player_stub(&player);

        let save_path = dir.join("assembly.usda");
        // Multiple levels → `plan_assembly` marks them all invisible.
        let plan = plan_assembly(
            &dir,
            &[hub.clone(), forest.clone(), hub2.clone()],
            Some(&PlayerRef::Project(player.clone())),
        );
        author_assembly_stage(&save_path, &plan, None, "Test Assembly")
            .expect("author assembly stage");

        let text = std::fs::read_to_string(&save_path).expect("read authored assembly");
        assert!(
            text.contains("stageKind = \"assembly\""),
            "missing assembly tag: {text}"
        );
        assert!(
            text.contains("def Scope \"Levels\""),
            "missing Levels scope: {text}"
        );
        // The obsolete `/World/XR` bootstrap must be gone.
        assert!(
            !text.contains("def Scope \"XR\""),
            "obsolete XR bootstrap scope must not be authored: {text}"
        );
        // Disambiguated holder names.
        assert!(text.contains("\"hub\""), "missing hub holder: {text}");
        assert!(text.contains("\"forest\""), "missing forest holder: {text}");
        assert!(text.contains("\"hub_2\""), "missing hub_2 holder: {text}");
        // Payloads authored relative to the assembly dir, held invisible.
        assert!(
            text.contains("payload = @levels/hub.usda@"),
            "missing hub payload arc: {text}"
        );
        assert!(
            text.contains("payload = @extra/hub.usda@"),
            "missing hub_2 payload arc: {text}"
        );
        assert!(
            text.contains("visibility = \"invisible\""),
            "levels must start invisible: {text}"
        );
        // Player composed as a `/World/Player` reference (not a sublayer).
        assert!(
            text.contains("def Xform \"Player\""),
            "missing /World/Player holder: {text}"
        );
        assert!(
            text.contains("references = @player.usda@</Player>"),
            "missing player reference arc: {text}"
        );
        assert!(
            !text.contains("subLayers"),
            "player must be a reference, not a sublayer: {text}"
        );
        // The composing overlay re-stamps the `idtx:playerRole = "local"` authority
        // seam on /World/Player (the role-neutral player asset carries none). Its
        // absence is what stops the XR eye camera and desktop runtime camera from
        // following the pawn, so lock it here.
        assert!(
            text.contains("token idtx:playerRole = \"local\""),
            "composed /World/Player must be stamped local: {text}"
        );

        // OpenUSD must re-compose the authored stage and project the holders plus
        // the referenced player prim.
        let mut stage = StageService::open_topology_only(&save_path).expect("re-open assembly");
        let names: Vec<String> = stage
            .project_topology()
            .expect("project topology")
            .iter()
            .map(|r| r.name.clone())
            .collect();
        for expected in ["World", "Levels", "hub", "forest", "hub_2", "Player"] {
            assert!(
                names.contains(&expected.to_string()),
                "authored assembly missing {expected}: {names:?}"
            );
        }
        // The player reference must actually open: its child (Head → Camera)
        // composes into /World/Player, proving the `</Player>` arc resolved.
        assert!(
            names.contains(&"Camera".to_string()),
            "player reference did not compose its children: {names:?}"
        );
        // The authored role must resolve on the *composed* /World/Player prim (not
        // just appear in the root-layer text), so hydration stamps PlayerRole::Local.
        let player_prim = stage
            .prim_at_path("/World/Player")
            .expect("composed player prim");
        assert_eq!(
            stage.read_player_role(player_prim).expect("read role"),
            Some("local".to_owned()),
            "composed /World/Player must resolve idtx:playerRole = local"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cook assembly marks exactly the default level visible: its holder has no
    /// `invisible` opinion while every other level does. Links `OpenUSD`.
    #[test]
    fn author_cook_assembly_default_level_is_only_visible() {
        let dir = scratch_dir("cookvis");
        let levels_dir = dir.join("levels");
        std::fs::create_dir_all(&levels_dir).unwrap();
        let a = levels_dir.join("a.usda");
        let b = levels_dir.join("b.usda");
        let c = levels_dir.join("c.usda");
        let player = dir.join("player.usda");
        for p in [&a, &b, &c] {
            write_stub_stage(p);
        }
        write_player_stub(&player);

        let save_path = dir.join("assembly-root.usda");
        // `b` is the default → the sole visible level.
        let plan = plan_cook_assembly(
            &dir,
            &[a.clone(), b.clone(), c.clone()],
            Some(b.as_path()),
            Some(&PlayerRef::Project(player.clone())),
        );
        assert_eq!(
            plan.levels.iter().filter(|l| l.visible).count(),
            1,
            "exactly one level visible"
        );
        assert!(
            plan.levels[1].visible,
            "default level `b` is the visible one"
        );

        author_assembly_stage(&save_path, &plan, None, "Cooked").expect("author cook assembly");
        let text = std::fs::read_to_string(&save_path).expect("read cook assembly");

        // Exactly two `invisible` opinions (a and c), none for b.
        assert_eq!(
            text.matches("visibility = \"invisible\"").count(),
            2,
            "only the two non-default levels hidden: {text}"
        );
        assert!(text.contains("stageKind = \"assembly\""));
        assert!(
            text.contains("references = @player.usda@</Player>"),
            "player reference present: {text}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A single visible level carrying a **root-level** `UsdPhysicsScene` (a
    /// sibling of the default prim) must have that scene re-referenced into the
    /// assembly root as a typeless `def "physicsScene" ( references =
    /// @level@</physicsScene> )`, since the default-prim payload drops it. Links
    /// `OpenUSD` (needs `IDTX_USD_DIR`).
    #[test]
    fn author_assembly_composes_root_level_physics_scene() {
        use idtx_usd::StageService;

        let dir = scratch_dir("physscene");
        let level = dir.join("level.usda");
        std::fs::write(
            &level,
            "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\
             def PhysicsScene \"physicsScene\"\n{\n    \
             vector3f physics:gravityDirection = (0, -1, 0)\n    \
             float physics:gravityMagnitude = 9.81\n}\n\
             def Xform \"World\"\n{\n    def Cube \"Box\" {}\n}\n",
        )
        .unwrap();

        let save_path = dir.join("assembly-root.usda");
        let plan = plan_assembly(&dir, std::slice::from_ref(&level), None);
        assert!(plan.levels[0].visible, "the lone level must be visible");
        author_assembly_stage(&save_path, &plan, None, "Phys").expect("author assembly");

        let text = std::fs::read_to_string(&save_path).expect("read assembly");
        assert!(
            text.contains("def \"physicsScene\""),
            "typeless physics-scene holder missing: {text}"
        );
        assert!(
            text.contains("</physicsScene>"),
            "scene reference must target the level's physicsScene prim: {text}"
        );

        // OpenUSD must re-compose a `PhysicsScene` prim at the assembly root.
        let mut stage = StageService::open_topology_only(&save_path).expect("re-open assembly");
        let scene = stage
            .project_topology()
            .expect("project topology")
            .iter()
            .find(|r| r.name == "physicsScene")
            .cloned()
            .expect("physicsScene prim composed");
        assert_eq!(
            scene.type_name, "PhysicsScene",
            "scene type must resolve through the reference"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A scene authored **inside** the default prim arrives with the level's
    /// default-prim payload, so the composer must NOT re-reference it (that would
    /// compose two scenes). Links `OpenUSD`.
    #[test]
    fn physics_scene_nested_in_world_is_not_duplicated() {
        let dir = scratch_dir("physnested");
        let level = dir.join("level.usda");
        std::fs::write(
            &level,
            "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\
             def Xform \"World\"\n{\n    def PhysicsScene \"physicsScene\" {}\n}\n",
        )
        .unwrap();

        let save_path = dir.join("assembly-root.usda");
        let plan = plan_assembly(&dir, std::slice::from_ref(&level), None);
        author_assembly_stage(&save_path, &plan, None, "Phys").expect("author assembly");

        let text = std::fs::read_to_string(&save_path).expect("read assembly");
        assert!(
            !text.contains("</physicsScene>"),
            "a scene under the default prim must not be re-referenced: {text}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A single visible level carrying a **root-level** stage behavior
    /// (`/StageBehavior`, a sibling of the default prim named by the root-layer
    /// `customLayerData["idtx:stageBehavior"]`) must have that prim surfaced as a
    /// nested **payload** under the level's own holder
    /// (`/World/Levels/<name>/StageBehavior`) — otherwise the default-prim payload
    /// drops it and the behavior-prepare pass never compiles/links its source. A
    /// payload (not a reference) keeps the compose context classified as payload.
    /// Links `OpenUSD` (needs `IDTX_USD_DIR`).
    #[test]
    fn author_assembly_composes_root_level_stage_behavior() {
        use idtx_usd::StageService;

        let dir = scratch_dir("stagebehavior");
        let level = dir.join("level.usda");
        std::fs::write(
            &level,
            "#usda 1.0\n(\n    defaultPrim = \"World\"\n    \
             customLayerData = {\n        \
             string \"idtx:stageBehavior\" = \"StageBehavior\"\n    }\n)\n\
             def Scope \"StageBehavior\" (\n    \
             prepend apiSchemas = [\"IdtxBehaviorAPI\"]\n)\n{\n    \
             uniform asset idtx:behavior:source = @./behaviors/stage.rs@\n}\n\
             def Xform \"World\"\n{\n    def Cube \"Box\" {}\n}\n",
        )
        .unwrap();

        let save_path = dir.join("assembly-root.usda");
        let plan = plan_assembly(&dir, std::slice::from_ref(&level), None);
        assert!(plan.levels[0].visible, "the lone level must be visible");
        let holder_name = plan.levels[0].holder_name.clone();
        author_assembly_stage(&save_path, &plan, None, "SB").expect("author assembly");

        let text = std::fs::read_to_string(&save_path).expect("read assembly");
        assert!(
            text.contains("</StageBehavior>"),
            "stage-behavior payload must target the level's /StageBehavior prim: {text}"
        );
        // It is a PAYLOAD arc (not a reference), nested under the level holder.
        assert!(
            text.contains("payload = @level.usda@</StageBehavior>"),
            "stage behavior must be a nested payload, not a reference: {text}"
        );

        // OpenUSD must re-compose the stage-behavior prim under the holder so the
        // behavior-prepare pass's topology traversal finds its source.
        let sb_path = format!("/World/Levels/{holder_name}/StageBehavior");
        let mut stage = StageService::open_topology_only(&save_path).expect("re-open assembly");
        assert!(
            stage.prim_at_path(&sb_path).is_some(),
            "{sb_path} must compose through the nested payload"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-end authoring of a **per-level assembly** (Scenario-2 composition):
    /// a level body re-rooted under `/World/level` as a reference, its root-level
    /// `StageBehavior` re-surfaced as a nested payload and `physicsScene` as a root
    /// reference, the player composed at `/World/Player` with the local-role seam,
    /// and a shared global sublayer whose prototype composes into the stage. Links
    /// `OpenUSD` (needs `IDTX_USD_DIR`).
    #[test]
    fn author_per_level_assembly_composes_body_player_globals_and_root_siblings() {
        use idtx_usd::StageService;

        let dir = scratch_dir("perlevel");
        let body = dir.join("level.usda");
        std::fs::write(
            &body,
            "#usda 1.0\n(\n    defaultPrim = \"World\"\n    \
             customLayerData = {\n        \
             string \"idtx:stageBehavior\" = \"StageBehavior\"\n    }\n)\n\
             def Scope \"StageBehavior\" (\n    \
             prepend apiSchemas = [\"IdtxBehaviorAPI\"]\n)\n{\n    \
             uniform asset idtx:behavior:source = @./behaviors/stage.rs@\n}\n\
             def PhysicsScene \"physicsScene\"\n{\n    \
             vector3f physics:gravityDirection = (0, -1, 0)\n    \
             float physics:gravityMagnitude = 9.81\n}\n\
             def Xform \"World\"\n{\n    def Cube \"Box\" {}\n}\n",
        )
        .unwrap();
        // A shared global sublayer contributing a prototype at stage root.
        let globals = dir.join("globals.usda");
        std::fs::write(
            &globals,
            "#usda 1.0\ndef Xform \"Prototypes\"\n{\n    def Xform \"Tree\" {}\n}\n",
        )
        .unwrap();
        let player = dir.join("player.usda");
        // A player with a `playerKind` variantSet (the built-in player has one), so
        // the desktop variant selection resolves instead of "variant restore
        // selection does not exist".
        std::fs::write(
            &player,
            "#usda 1.0\n(\n    defaultPrim = \"Player\"\n)\n\
             def Xform \"Player\" (\n    \
             prepend variantSets = \"playerKind\"\n    \
             variants = {\n        string playerKind = \"desktop\"\n    }\n)\n{\n    \
             variantSet \"playerKind\" = {\n        \
             \"desktop\" {\n            \
             def Xform \"Head\"\n            {\n                def Camera \"Camera\" {}\n            }\n        }\n        \
             \"xr\" {\n            \
             def Xform \"Head\"\n            {\n                def Camera \"Camera\" {}\n            }\n        }\n    }\n}\n",
        )
        .unwrap();

        let save_path = dir.join("level-assembly.usda");
        let plan = PerLevelAssembly {
            body_arc: "level.usda".to_owned(),
            player_reference: Some("player.usda".to_owned()),
            player_variant: Some("desktop".to_owned()),
            shared_sublayers: vec!["globals.usda".to_owned()],
        };
        author_per_level_assembly(&save_path, &plan, &body, None, "Forest")
            .expect("author per-level assembly");

        let text = std::fs::read_to_string(&save_path).expect("read authored assembly");
        assert!(text.contains("stageKind = \"assembly\""), "{text}");
        // Shared globals authored as a root subLayer.
        assert!(
            text.contains("subLayers") && text.contains("globals.usda"),
            "shared sublayer must be authored: {text}"
        );
        // Body is a REFERENCE (not a payload); no "Levels" scope.
        assert!(
            text.contains("references = @level.usda@\n") || text.contains("references = @level.usda@"),
            "body must be composed as a reference: {text}"
        );
        assert!(
            !text.contains("def Scope \"Levels\""),
            "per-level assembly must omit the Levels scope: {text}"
        );
        // StageBehavior re-surfaced as a PAYLOAD (so its compose context runs).
        assert!(
            text.contains("payload = @level.usda@</StageBehavior>"),
            "stage behavior must be a nested payload: {text}"
        );
        // Physics scene re-referenced at the assembly root.
        assert!(
            text.contains("references = @level.usda@</physicsScene>"),
            "root physics scene must be re-referenced: {text}"
        );
        assert!(
            text.contains("token idtx:playerRole = \"local\""),
            "composed /World/Player must be stamped local: {text}"
        );

        // OpenUSD must re-compose the authored stage.
        let mut stage = StageService::open_topology_only(&save_path).expect("re-open assembly");
        let names: Vec<String> = stage
            .project_topology()
            .expect("project topology")
            .iter()
            .map(|r| r.name.clone())
            .collect();
        for expected in [
            "World",
            "level",
            "Player",
            "Camera",       // player reference composed its children
            "StageBehavior",
            "physicsScene",
            "Prototypes",   // from the shared global sublayer
            "Tree",
            "Box",          // the body's content
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "authored per-level assembly missing {expected}: {names:?}"
            );
        }
        // The body is re-rooted under /World/level (not /World).
        assert!(
            stage.prim_at_path("/World/level/Box").is_some(),
            "body content must compose under /World/level"
        );
        assert!(
            stage.prim_at_path("/World/Box").is_none(),
            "body content must NOT compose at /World root"
        );
        assert!(
            stage.prim_at_path("/World/level/StageBehavior").is_some(),
            "re-surfaced stage behavior must compose under the holder"
        );
        // The physics scene resolves its type through the root reference.
        let scene = stage
            .project_topology()
            .expect("project topology")
            .iter()
            .find(|r| r.name == "physicsScene")
            .cloned()
            .expect("physicsScene prim composed");
        assert_eq!(scene.type_name, "PhysicsScene", "scene type resolves");
        // The authored role resolves on the composed player prim.
        let player_prim = stage
            .prim_at_path("/World/Player")
            .expect("composed player prim");
        assert_eq!(
            stage.read_player_role(player_prim).expect("read role"),
            Some("local".to_owned()),
            "composed /World/Player must resolve idtx:playerRole = local"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shadow_mode_tokens_match_shim() {
        // Tokens must mirror `author_root_shadow_mode` in the C++ shim.
        assert_eq!(shadow_mode_token(ShadowMode::Off), None);
        assert_eq!(shadow_mode_token(ShadowMode::Hard), Some("hard"));
        assert_eq!(shadow_mode_token(ShadowMode::Smooth), Some("smooth"));
        assert_eq!(shadow_mode_token(ShadowMode::Soft), Some("soft"));
    }
}
