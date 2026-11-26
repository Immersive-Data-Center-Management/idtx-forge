//! Compile authored behavior **sources** (`idtx:behavior:source` → `.rs`) to
//! `.wasm` before a runtime preview and at packaging, and link the compiled
//! module to its prim — **without ever mutating the authored (source) stage**.
//!
//! A Conductor-created behavior carries only its Rust source on-USD
//! (`idtx:behavior:source` + `idtx:behavior:uuid` + `idtx:behavior:kind`, with
//! the `IdtxBehaviorAPI` schema applied); the `.wasm` is a build artifact
//! produced here. This module:
//!
//! 1. [`prepare`] — opens an entry/world stage read-only, enumerates every prim
//!    with an authored `idtx:behavior:source`, resolves that `.rs` (must live
//!    inside the project), and compiles it to `<project>/behaviors/<name>.wasm`
//!    via [`idtx_behavior_build::build_behavior`] (source-hash cached). It is
//!    **fail-closed**: any compile error aborts the whole pass so a broken player
//!    is never launched/packaged.
//! 2. [`author_module_overlay`] — copies the entry stage's *root layer* to a
//!    timestamped sibling temp (never flattening references/payloads), then
//!    applies `IdtxBehaviorAPI` and authors `idtx:behavior:module = <module>`
//!    onto each prepared prim on that temp. The temp is used as the world
//!    sublayer for a preview or as the cook `entry_stage` for packaging; it is
//!    deleted when the returned guard drops.
//!
//! Mirrors the sibling **idtx-poc** `behavior_package::prepare` flow, adapted to
//! studio's attachment model (the compiled module lives on `idtx:behavior:module`,
//! the same attribute `attach_behavior` authors — see the hydrator read path).

use std::path::{Path, PathBuf};

use idtx_behavior_build::{BuildOptions, build_behavior};
use idtx_usd::{PropertyValue, StageService, StructuralCommand};

/// USD type name of the behavior `HasAPI` schema (`idtx_usd_extension::
/// BEHAVIOR_SCHEMA`), applied to each prepared prim so `HasAPI` recognises it.
/// Kept as a literal so the shell need not depend on `idtx-scene` /
/// `idtx-usd-extension` for the one name (mirrors `idtx_scene::behavior::attr::
/// SCHEMA`).
const BEHAVIOR_SCHEMA: &str = "IdtxBehaviorAPI";

/// The authored Rust-source attribute (`idtx:behavior:source`), whose value is
/// the author-facing `.rs` path. Kept as a literal so the shell need not depend
/// on `idtx-scene` for the one constant (mirrors `behavior_tools`'s attachment
/// literal and `idtx_scene::behavior::attr::SOURCE`).
const BEHAVIOR_SOURCE: &str = "idtx:behavior:source";

/// The compiled-module attribute (`idtx:behavior:module`), the `.wasm` link
/// authored onto each prepared prim. Mirrors `idtx_scene::behavior::attr::MODULE`.
const BEHAVIOR_MODULE: &str = "idtx:behavior:module";

/// One prim whose `idtx:behavior:source` was compiled to a `.wasm` module.
pub(crate) struct PreparedBehavior {
    /// Absolute prim path the module attaches to (e.g. `/World/Cube`).
    pub prim_path: String,
    /// Absolute path to the compiled `.wasm` (under `<project>/behaviors`).
    pub wasm_abs: PathBuf,
}

/// The result of a compile pass over an entry/world stage.
pub(crate) struct PreparedBehaviors {
    pub items: Vec<PreparedBehavior>,
}

impl PreparedBehaviors {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Discover every prim on `entry_stage` carrying an authored `idtx:behavior:source`
/// and compile that `.rs` to `.wasm` under `<project_root>/behaviors`.
///
/// `sdk_path` is the staged guest-ABI SDK sysroot (from
/// [`behavior_tools::discover_sdk_path`]); it may be `None`, in which case the
/// pass succeeds only for a stage with **no** behavior sources — a source that
/// needs compiling without a resolvable SDK is a fail-closed error. Returns the
/// prim→module links.
///
/// # Errors
///
/// Fails (fail-closed) when the stage cannot be opened, a source path escapes
/// `project_root`, the SDK is missing while a source needs compiling, or any
/// behavior fails to compile.
pub(crate) fn prepare(
    entry_stage: &Path,
    project_root: &Path,
    sdk_path: Option<&Path>,
) -> Result<PreparedBehaviors, String> {
    let mut stage = StageService::open(entry_stage)
        .map_err(|e| format!("open stage {}: {e}", idtx_project::normalize_display_path(entry_stage)))?;
    let behaviors_out = project_root.join("behaviors");

    // Collect prim ids up front so the topology borrow is released before the
    // per-prim `inspect_prim` (both take `&mut stage`).
    let prims: Vec<idtx_usd::PrimId> = stage
        .project_topology()
        .map_err(|e| format!("project topology: {e}"))?
        .iter()
        .map(|r| r.prim)
        .collect();

    let mut items = Vec::new();
    let mut compiled: std::collections::BTreeMap<PathBuf, PathBuf> = std::collections::BTreeMap::new();
    for prim in prims {
        let props = stage
            .inspect_prim(prim)
            .map_err(|e| format!("inspect prim: {e}"))?;
        let has_source = props
            .iter()
            .any(|p| p.name == BEHAVIOR_SOURCE && p.authored);
        if !has_source {
            continue;
        }

        let prim_path = stage
            .path_for(prim)
            .map(str::to_owned)
            .ok_or_else(|| "prim path unavailable for authored behavior source".to_owned())?;

        eprintln!("[preview] [Prepare] Found Prim with authored behavior: {prim_path}");

        // Resolve the `.rs` through USD's own asset resolution, which anchors the
        // authored (layer-relative) path against its *authoring* layer. This is
        // the crux of the source/module split: whether the prim is reached
        // directly (preview flattens the open level into `stages/`) or through
        // the composed assembly (packaging's entry stage payloads the level from
        // the project root), `@./behaviors/x.rs@` resolves to the same file next
        // to the level that authored it. A plain relative join against the entry
        // stage's dir would diverge between the two flows.
        let rs_abs = stage
            .resolve_prim_asset(prim, BEHAVIOR_SOURCE)
            .map_err(|e| format!("resolve behavior source for {prim_path}: {e}"))?
            .map(PathBuf::from)
            .ok_or_else(|| format!("behavior source for {prim_path} could not be resolved"))?;
        // A behavior source must live inside the project **or** inside the
        // toolbox's built-in asset tree (the built-in player ships its default
        // gameplay behaviors as `.rs` there, referenced via `@built-in/...@`).
        // Any source outside both is rejected (fail-closed).
        if !is_inside(project_root, &rs_abs) && !is_inside(&idtx_assets::builtin_tree(), &rs_abs) {
            return Err(format!(
                "behavior source {} for {prim_path} is outside the project and the built-in tree",
                idtx_project::normalize_display_path(&rs_abs)
            ));
        }
        if !rs_abs.is_file() {
            return Err(format!(
                "behavior source {} for {prim_path} does not exist",
                idtx_project::normalize_display_path(&rs_abs)
            ));
        }

        let sdk = sdk_path.ok_or_else(|| {
            format!(
                "behavior SDK not found; cannot compile source for {prim_path} \
                 (set IDTX_BEHAVIOR_SDK or ship the staged sysroot)"
            )
        })?;
        let source = std::fs::canonicalize(&rs_abs).map_err(|e| format!("resolve source for {prim_path}: {e}"))?;
        let wasm_abs = if let Some(wasm) = compiled.get(&source) {
            wasm.clone()
        } else {
            let artifact = build_behavior(&source, &BuildOptions::new(&behaviors_out, sdk))
                .map_err(|e| format!("compile behavior for {prim_path}: {e}"))?;
            compiled.insert(source, artifact.wasm_path.clone());
            artifact.wasm_path
        };
        items.push(PreparedBehavior {
            prim_path,
            wasm_abs,
        });
    }

    Ok(PreparedBehaviors { items })
}

/// A temp stage carrying the authored `idtx:behavior:module` links, deleted when
/// dropped. Its [`path`](AuthoredOverlay::path) is a timestamped sibling of the
/// source stage (so the copied root layer's relative references still resolve).
#[derive(Debug)]
pub(crate) struct AuthoredOverlay {
    path: PathBuf,
}

impl AuthoredOverlay {
    /// The authored temp stage path (use as a preview world layer or cook entry).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for AuthoredOverlay {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Copy `entry_stage`'s root layer to a timestamped sibling temp and author
/// `idtx:behavior:module = <module>` onto each prepared prim (relative to the
/// temp dir, so the module resolves against the composed/packaged stage). The
/// behavior's capability manifest (capabilities, inputs, …) rides along on the
/// copied root layer as `IdtxBehaviorAPI` schema attributes — there is no
/// sidecar `idtx.toml`. The source stage is never touched.
///
/// # Errors
///
/// Fails when the stage cannot be opened, the root layer cannot be exported, or
/// a module attribute cannot be authored.
pub(crate) fn author_module_overlay(
    entry_stage: &Path,
    prepared: &PreparedBehaviors,
) -> Result<AuthoredOverlay, String> {
    let temp = temp_sibling(entry_stage);

    // Copy ONLY the authored root layer (never flatten references/payloads).
    {
        let stage = StageService::open(entry_stage)
            .map_err(|e| format!("open stage {}: {e}", idtx_project::normalize_display_path(entry_stage)))?;
        stage
            .export_root_layer(&temp)
            .map_err(|e| format!("export root layer to {}: {e}", idtx_project::normalize_display_path(&temp)))?;
    }

    let overlay = AuthoredOverlay { path: temp.clone() };
    let temp_dir = temp
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);

    let mut stage = StageService::open(&temp).map_err(|e| format!("reopen temp stage: {e}"))?;
    for behavior in &prepared.items {
        let Some(prim) = stage.prim_at_path(&behavior.prim_path) else {
            // The prim vanished between prepare and author — skip rather than fail.
            continue;
        };
        let asset = relative_asset(&temp_dir, &behavior.wasm_abs);
        // Apply the IdtxBehaviorAPI schema, then author the compiled module link.
        stage
            .apply_structural_command(&StructuralCommand::ApplyApi {
                path: behavior.prim_path.clone(),
                schema: BEHAVIOR_SCHEMA.to_owned(),
            })
            .map_err(|e| format!("apply IdtxBehaviorAPI on {}: {e}", behavior.prim_path))?;
        stage
            .apply_structural_command(&StructuralCommand::SetProperty {
                prim,
                property: BEHAVIOR_MODULE.to_owned(),
                value: PropertyValue::Asset(asset),
            })
            .map_err(|e| format!("author module on {}: {e}", behavior.prim_path))?;
    }
    // Persist the authored links back onto the temp root layer.
    stage
        .export_root_layer(&temp)
        .map_err(|e| format!("save authored overlay: {e}"))?;

    Ok(overlay)
}

/// Co-located variant of [`author_module_overlay`] for the per-level cook.
///
/// Copies each compiled `.wasm` into `<assembly_dir>/behaviors/<name>.wasm` (next
/// to the composed per-level assembly) and authors
/// `idtx:behavior:module = ./behaviors/<name>.wasm` — a path **relative to the
/// assembly dir**. `idtx_usd::package_stage` preserves that relative value
/// verbatim through the per-level flatten, so at runtime the module resolves
/// layer-relative to the flattened `level.usdz`'s own directory, which is exactly
/// where `idtx_build::cook` co-locates the same `.wasm`.
///
/// Returns the overlaid stage path (a plain sibling temp of `entry_stage` — **not**
/// a self-deleting [`AuthoredOverlay`], since the caller's per-level cook owns the
/// enclosing scratch dir and removes the whole tree after flattening) and the list
/// of co-located `.wasm` paths for the cook to copy beside `level.usdz`.
///
/// # Errors
///
/// Fails when a `.wasm` cannot be copied beside the assembly, the stage cannot be
/// opened / its root layer exported, or a module attribute cannot be authored.
pub(crate) fn author_colocated_module_overlay(
    entry_stage: &Path,
    prepared: &PreparedBehaviors,
) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let asm_dir = entry_stage
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let behaviors_dir = asm_dir.join("behaviors");
    std::fs::create_dir_all(&behaviors_dir)
        .map_err(|e| format!("create {}: {e}", idtx_project::normalize_display_path(&behaviors_dir)))?;

    // Co-locate each compiled module beside the assembly (keyed by prim) so the
    // authored module path is `./behaviors/<name>.wasm` relative to the assembly.
    let mut links: Vec<(String, PathBuf)> = Vec::new();
    let mut wasm: Vec<PathBuf> = Vec::new();
    for behavior in &prepared.items {
        let name = behavior
            .wasm_abs
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("compiled module has no file name: {}", behavior.wasm_abs.display()))?;
        let dst = behaviors_dir.join(name);
        if dst != behavior.wasm_abs {
            std::fs::copy(&behavior.wasm_abs, &dst)
                .map_err(|e| format!("co-locate {} → {}: {e}", behavior.wasm_abs.display(), dst.display()))?;
        }
        if !wasm.contains(&dst) {
            wasm.push(dst.clone());
        }
        links.push((behavior.prim_path.clone(), dst));
    }

    let temp = temp_sibling(entry_stage);
    // Copy ONLY the authored root layer (never flatten references/payloads).
    {
        let stage = StageService::open(entry_stage)
            .map_err(|e| format!("open stage {}: {e}", idtx_project::normalize_display_path(entry_stage)))?;
        stage
            .export_root_layer(&temp)
            .map_err(|e| format!("export root layer to {}: {e}", idtx_project::normalize_display_path(&temp)))?;
    }

    let mut stage = StageService::open(&temp).map_err(|e| format!("reopen temp stage: {e}"))?;
    for (prim_path, wasm_abs) in &links {
        let Some(prim) = stage.prim_at_path(prim_path) else {
            // The prim vanished between prepare and author — skip rather than fail.
            continue;
        };
        let asset = relative_asset(&asm_dir, wasm_abs);
        stage
            .apply_structural_command(&StructuralCommand::ApplyApi {
                path: prim_path.clone(),
                schema: BEHAVIOR_SCHEMA.to_owned(),
            })
            .map_err(|e| format!("apply IdtxBehaviorAPI on {prim_path}: {e}"))?;
        stage
            .apply_structural_command(&StructuralCommand::SetProperty {
                prim,
                property: BEHAVIOR_MODULE.to_owned(),
                value: PropertyValue::Asset(asset),
            })
            .map_err(|e| format!("author module on {prim_path}: {e}"))?;
    }
    stage
        .export_root_layer(&temp)
        .map_err(|e| format!("save authored overlay: {e}"))?;

    Ok((temp, wasm))
}

/// Whether `path` lives inside `root` (after best-effort canonicalization).
fn is_inside(root: &Path, path: &Path) -> bool {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    path.starts_with(&root)
}

/// A timestamped sibling temp path for the authored overlay, in the same dir as
/// `stage` so the copied root layer's relative references resolve unchanged.
fn temp_sibling(stage: &Path) -> PathBuf {
    let dir = stage.parent().unwrap_or_else(|| Path::new("."));
    let stem = stage
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("stage");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    dir.join(format!(
        ".idtx-behavior-{stem}-{}-{nanos}.usda",
        std::process::id()
    ))
}

/// Compute the asset-path string to author for `wasm_abs`, relative to `base`
/// (the stage directory) with forward slashes and a leading `./`; falls back to
/// the absolute forward-slash path when the two share no common root.
///
/// The absolute fallback strips any Windows verbatim (`\\?\`) prefix that
/// `canonicalize` leaves on, so the authored asset path is a plain
/// `C:/…/foo.wasm` rather than `//?/C:/…/foo.wasm` (the latter is opaque to the
/// resolver and leaks into the composed assembly).
fn relative_asset(base: &Path, wasm_abs: &Path) -> String {
    let base_c = idtx_project::strip_verbatim_prefix(
        &std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf()),
    );
    let wasm_c = idtx_project::strip_verbatim_prefix(
        &std::fs::canonicalize(wasm_abs).unwrap_or_else(|_| wasm_abs.to_path_buf()),
    );
    if let Ok(rel) = wasm_c.strip_prefix(&base_c) {
        let joined = rel.to_string_lossy().replace('\\', "/");
        format!("./{joined}")
    } else {
        wasm_c.to_string_lossy().replace('\\', "/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_asset_prefixes_dot_slash_and_forward_slashes() {
        let dir = std::env::temp_dir().join("idtx-relasset-test");
        let sub = dir.join("behaviors");
        std::fs::create_dir_all(&sub).unwrap();
        let wasm = sub.join("cube_behavior.wasm");
        std::fs::write(&wasm, b"\0asm").unwrap();

        let asset = relative_asset(&dir, &wasm);
        assert_eq!(asset, "./behaviors/cube_behavior.wasm");
        assert!(!asset.contains('\\'));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_asset_falls_back_to_absolute_when_no_common_root() {
        // A path with no shared prefix returns the absolute forward-slash form.
        let base = std::env::temp_dir().join("idtx-relasset-base");
        std::fs::create_dir_all(&base).unwrap();
        let elsewhere = std::env::temp_dir().join("idtx-relasset-other/x.wasm");
        std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
        std::fs::write(&elsewhere, b"\0asm").unwrap();

        let asset = relative_asset(&base, &elsewhere);
        assert!(asset.ends_with("idtx-relasset-other/x.wasm"), "got {asset}");
        assert!(!asset.contains('\\'));

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("idtx-relasset-other"));
    }

    #[test]
    fn is_inside_detects_containment() {
        let root = std::env::temp_dir().join("idtx-inside-root");
        let inside = root.join("behaviors/x.rs");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        std::fs::write(&inside, b"//").unwrap();
        assert!(is_inside(&root, &inside));

        let outside = std::env::temp_dir().join("idtx-inside-other.rs");
        std::fs::write(&outside, b"//").unwrap();
        assert!(!is_inside(&root, &outside));

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn temp_sibling_is_in_same_dir_with_usda_ext() {
        let stage = Path::new("/tmp/project/stages/main.usd");
        let temp = temp_sibling(stage);
        assert_eq!(temp.parent(), stage.parent());
        assert_eq!(temp.extension().and_then(|e| e.to_str()), Some("usda"));
        assert!(
            temp.file_name()
                .and_then(|n| n.to_str())
                .unwrap()
                .starts_with(".idtx-behavior-main-")
        );
    }
}
