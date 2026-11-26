//! Conductor-side behavior *authoring* tooling for the Behavior inspector
//! panel — the imperative glue behind its **Edit Code** and **Cargo Check**
//! buttons.
//!
//! Modelled on the sibling **IDTX-POC** `behavior_host` / `behavior_editor`
//! modules, trimmed to what idtx-studio's attachment model needs:
//!
//! - [`edit_behavior_source`] resolves the original author source and prepares
//!   its persistent Cargo/VS Code development workspace in a background job.
//! - [`start_check`] spawns a background `cargo check --target wasm32-…` and
//!   publishes the result back to the inspector panel via
//!   [`idtx_editor_ui::inspector::panels::behavior::set_behavior_check`];
//!   [`poll_checks`] drains finished jobs once per frame.
//!
//! **Explicit non-goal.** Nothing here *plays or runs* a behavior. This is a
//! pure authoring + tooling surface; the lifecycle hooks are driven by the
//! transport/session and the standalone runtime, and playback ownership stays
//! there.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, TryRecvError};

use idtx_behavior::{Capabilities, verify_link};
use idtx_behavior_build::{BehaviorSource, BuildOptions, build_behavior, check_behavior, resolve_sdk_dir};
#[cfg(feature = "authoring")]
use idtx_behavior_build::{render_author_skeleton, synthesize_uuid};
use idtx_editor_ui::inspector::panels::behavior::{
    BehaviorCheckState, BehaviorVerifyState, LinkSummary, set_behavior_check, set_behavior_verify,
};
use idtx_studio_session::StudioSession;

/// The `Asset`-typed compiled-module attribute (`idtx:behavior:module`), whose
/// value is the module `.wasm` asset path. Kept as a literal so the shell need
/// not depend on `idtx-scene` for the one constant (mirrors
/// `idtx_scene::behavior::attr::MODULE`).
const BEHAVIOR_ATTACHMENT: &str = "idtx:behavior:module";

/// A background compile-check job: the prim path it targets + the channel the
/// worker reports its `Ok(())` / `Err(diagnostics)` result on.
struct CheckJob {
    target_path: String,
    rx: Receiver<Result<(), String>>,
}

/// Process-global queue of in-flight compile-check jobs. The single-threaded UI
/// thread pushes on [`start_check`] and drains on [`poll_checks`]; the `Mutex`
/// only guards against a poisoned re-entry, never real cross-thread contention.
static CHECK_JOBS: Mutex<Vec<CheckJob>> = Mutex::new(Vec::new());

/// A background link-verification job: the prim path it targets + the channel
/// the worker reports its [`LinkSummary`] / error on.
struct VerifyJob {
    target_path: String,
    rx: Receiver<Result<LinkSummary, String>>,
}

/// Process-global queue of in-flight verify jobs (sibling of [`CHECK_JOBS`]).
static VERIFY_JOBS: Mutex<Vec<VerifyJob>> = Mutex::new(Vec::new());

/// Open the attached behavior's Rust **source** for `prim_path` in an editor.
///
/// Uses USD's authored-layer resolution and analyzes that original source, not
/// a generated copy. Cargo preparation/profile setup runs off the UI thread.
pub(crate) fn edit_behavior_source(
    ctx: &egui::Context,
    session: &mut StudioSession,
    prim_path: &str,
    project_root: &Path,
    action: crate::behavior_editor::EditorAction,
) -> Result<(), String> {
    let src = resolve_behavior_source(session, prim_path)
        .ok_or_else(|| "No behavior source could be resolved for this prim".to_owned())?;
    if !src.is_file() {
        return Err(format!("Behavior source not found: {}", src.display()));
    }
    let sdk = discover_sdk_path().ok_or("Could not locate idtx-behavior-sdk (set IDTX_BEHAVIOR_SDK)")?;
    crate::behavior_editor::start(ctx, session.id().get(), prim_path, project_root.to_path_buf(), src, sdk, action);
    Ok(())
}

/// Create a **source-authored** behavior for `prim_path` (Conductor "Create
/// Behavior").
///
/// Writes an entry module in a named source folder under `behaviors/` next
/// to the edited stage (the level's own directory), and authors the *source*
/// triple (`idtx:behavior:source` / `idtx:behavior:uuid` / `idtx:behavior:kind`)
/// onto the prim with a stage-relative path (`@./behaviors/<stem>/behavior.rs@`). Because
/// the opinion is authored on (and thus anchored to) the level layer, USD
/// resolves it to the same file whether the level is opened directly (preview)
/// or composed through the assembly (packaging) — see
/// [`crate::behavior_prepare::prepare`]. The caller opens the editor workspace. **No
/// `.wasm` is produced** — the module is cross-compiled before preview / at
/// packaging, into an overlay, so the source stage stays source-only.
///
/// Returns the authored source path on success so the
/// caller can kick a non-blocking [`start_check`]. Errors are human-facing.
#[cfg(feature = "authoring")]
pub(crate) fn create_behavior_source(
    session: &mut StudioSession,
    prim_path: &str,
) -> Result<String, String> {
    if prim_path.is_empty() || prim_path == "/" {
        return Err("Select a prim to create a behavior on".to_owned());
    }

    // `behaviors/` sits next to the edited stage (the authoring layer for the
    // source opinion), so `@./behaviors/<stem>/behavior.rs@` resolves against that layer
    // in every flow. The compiled `.wasm` is emitted elsewhere (the project's
    // `behaviors/`, where the cook sweeps) at preview/package time.
    let behaviors_dir = stage_dir(session).join("behaviors");
    std::fs::create_dir_all(&behaviors_dir)
        .map_err(|e| format!("Could not create behaviors/: {e}"))?;

    // Derive a collision-free stem from the prim's leaf name.
    let base = sanitize_stem(prim_leaf(prim_path));
    let stem = unique_stem(&behaviors_dir, &base);
    let folder = behaviors_dir.join(&stem);
    std::fs::create_dir(&folder).map_err(|e| format!("Could not create {}: {e}", folder.display()))?;
    let rs_path = folder.join("behavior.rs");

    let source = render_author_skeleton(&stem, prim_path, "runtime");
    let uuid = synthesize_uuid(&stem, &source);
    if let Err(e) = std::fs::write(&rs_path, &source) {
        let _ = std::fs::remove_dir(&folder);
        return Err(format!("Could not write {}: {e}", rs_path.display()));
    }

    // Author the source triple onto the prim (current edit target). The behavior
    // manifest (capabilities, inputs, …) lives on the IdtxBehaviorAPI schema
    // attributes — authored in the inspector's config area, not a sidecar
    // `idtx.toml`. On failure, roll back the `.rs` we just wrote.
    let source_asset = format!("./behaviors/{stem}/behavior.rs");
    if let Err(e) = session.create_behavior_source(prim_path, &uuid, "runtime", &source_asset) {
        let _ = std::fs::remove_dir_all(&folder);
        return Err(format!("Failed to author behavior attributes: {e}"));
    }

    Ok(source_asset)
}

/// **Attach** an existing Rust behavior **source** to `prim_path` (Conductor
/// "Attach Behavior…").
///
/// An in-project entry is reused, so attachments can share an implementation.
/// External entries import the selected source collection, preserving nested
/// modules, shared source and include data, then author a layer-relative source
/// reference. Capability/input configuration stays on the USD schema.
///
/// **No `.wasm` is produced** — the module is cross-compiled before preview / at
/// packaging, into an overlay (mirrors [`create_behavior_source`]). Returns the
/// authored layer-relative source path on success so the caller can
/// kick a non-blocking [`start_check`]. Errors are human-facing.
#[cfg(feature = "authoring")]
pub(crate) fn attach_behavior_source(
    session: &mut StudioSession,
    prim_path: &str,
    picked_rs: &Path,
    project_root: &Path,
    collection_root: Option<&Path>,
) -> Result<String, String> {
    if prim_path.is_empty() || prim_path == "/" {
        return Err("Select a prim to attach a behavior on".to_owned());
    }

    let collection = BehaviorSource::discover(picked_rs, collection_root).map_err(|e| e.to_string())?;
    let source = std::fs::read_to_string(collection.entry())
        .map_err(|e| format!("Could not read {}: {e}", picked_rs.display()))?;
    let kind = parse_source_kind(&source);

    let project_root = std::fs::canonicalize(project_root).map_err(|e| e.to_string())?;
    let project_root = idtx_project::strip_verbatim_prefix(&project_root);
    if collection.entry().starts_with(&project_root) {
        let source_asset = source_asset_path(&stage_dir(session), collection.entry())?;
        let name = source_display_stem(collection.entry());
        let uuid = synthesize_uuid(&collection.name(), &source);
        session.attach_behavior_source(prim_path, &source_asset, &uuid, kind, &name)
            .map_err(|e| format!("Failed to author behavior attributes: {e}"))?;
        return Ok(source_asset);
    }

    let behaviors_dir = stage_dir(session).join("behaviors");
    std::fs::create_dir_all(&behaviors_dir)
        .map_err(|e| format!("Could not create behaviors/: {e}"))?;

    let base = source_display_stem(collection.entry());
    let stem = unique_stem(&behaviors_dir, &base);
    let folder = behaviors_dir.join(&stem);
    let rs_path = collection.copy_to(&folder).map_err(|e| format!("Could not import source collection: {e}"))?;

    let uuid = synthesize_uuid(&stem, &source);
    let source_asset = match source_asset_path(&stage_dir(session), &rs_path) {
        Ok(asset) => asset,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&folder);
            return Err(error);
        }
    };
    if let Err(e) = session.attach_behavior_source(prim_path, &source_asset, &uuid, kind, &stem) {
        let _ = std::fs::remove_dir_all(&folder);
        return Err(format!("Failed to author behavior attributes: {e}"));
    }

    Ok(source_asset)
}

#[cfg(feature = "authoring")]
fn source_display_stem(entry: &Path) -> String {
    let stem = entry.file_stem().and_then(|s| s.to_str()).unwrap_or("behavior");
    let stem = if stem == "behavior" {
        entry.parent().and_then(Path::file_name).and_then(|s| s.to_str()).unwrap_or(stem)
    } else { stem };
    sanitize_file_stem(stem)
}

/// A new local implementation, including all supported source inputs, with the
/// composed behavior config retained. Also works for built-in/inherited code.
#[cfg(feature = "authoring")]
pub(crate) fn duplicate_behavior_source(session: &mut StudioSession, prim_path: &str) -> Result<String, String> {
    let source = resolve_behavior_source(session, prim_path).ok_or("No behavior source could be resolved")?;
    let collection = BehaviorSource::discover(&source, None).map_err(|e| e.to_string())?;
    let config = session.read_composed_behavior_config(prim_path).ok_or("Behavior config could not be read")?;
    let before = session.root_layer_snapshot().map_err(|e| e.to_string())?;
    let behaviors = stage_dir(session).join("behaviors");
    std::fs::create_dir_all(&behaviors).map_err(|e| e.to_string())?;
    let stem = unique_stem(&behaviors, &format!("{}_copy", source_display_stem(&source)));
    let folder = behaviors.join(&stem);
    let entry = collection.copy_to(&folder).map_err(|e| e.to_string())?;
    let result = (|| {
        let source_asset = source_asset_path(&stage_dir(session), &entry)?;
        let source = std::fs::read_to_string(&entry).map_err(|e| e.to_string())?;
        let uuid = synthesize_uuid(&source_asset, &source);
        session.attach_behavior_source(prim_path, &source_asset, &uuid, parse_source_kind(&source), &stem).map_err(|e| e.to_string())?;
        session.apply_behavior_config(prim_path, &config).map_err(|e| e.to_string())?;
        Ok(source_asset)
    })();
    if result.is_err() {
        let _ = session.restore_root_layer_snapshot(&before);
        let _ = std::fs::remove_dir_all(folder);
    }
    result
}

/// Relative to the actual authoring layer, including `..` for a source shared
/// across levels. No machine-local absolute paths are persisted in USD.
#[cfg(feature = "authoring")]
fn source_asset_path(base: &Path, source: &Path) -> Result<String, String> {
    let base = idtx_project::strip_verbatim_prefix(&std::fs::canonicalize(base).map_err(|e| e.to_string())?);
    let source = idtx_project::strip_verbatim_prefix(&std::fs::canonicalize(source).map_err(|e| e.to_string())?);
    let base_parts: Vec<_> = base.components().collect();
    let source_parts: Vec<_> = source.components().collect();
    let common = base_parts.iter().zip(&source_parts).take_while(|(a, b)| a == b).count();
    if common == 0 { return Err("Behavior source must be on the same filesystem as its stage".into()); }
    let mut relative = PathBuf::from(".");
    for _ in common..base_parts.len() { relative.push(".."); }
    for component in &source_parts[common..] { relative.push(component.as_os_str()); }
    relative.to_str().map(|s| s.replace('\\', "/")).ok_or("Behavior source path is not UTF-8".into())
}

/// Read the execution kind from a behavior source's `#[behavior(kind = "…")]`
/// attribute. Returns `"editor"` when that attribute names the editor kind,
/// else `"runtime"` (the macro's default, and the fallback when no `#[behavior]`
/// attribute is present). Matches the only machine-readable manifest datum the
/// `#[behavior]` macro accepts (`idtx-behavior-macros`).
#[cfg(feature = "authoring")]
fn parse_source_kind(source: &str) -> &'static str {
    // Isolate the `#[behavior( … )]` attribute body so a `kind = "editor"` in,
    // say, a comment or string elsewhere does not mislead the scan.
    if let Some(start) = source.find("#[behavior") {
        let tail = &source[start..];
        let body = tail
            .find(']')
            .map_or(tail, |end| &tail[..end]);
        if body.contains("editor") {
            return "editor";
        }
    }
    "runtime"
}

/// Sanitise a picked source file stem into a lower-snake behavior file stem,
/// preserving the author's own name (unlike [`sanitize_stem`], which derives a
/// `<prim>_behavior` stem). Non-alphanumerics collapse to a single `_`; an empty
/// result falls back to `"behavior"`.
#[cfg(feature = "authoring")]
fn sanitize_file_stem(stem: &str) -> String {
    let mut s: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    while s.contains("__") {
        s = s.replace("__", "_");
    }
    let s = s.trim_matches('_').to_owned();
    if s.is_empty() {
        "behavior".to_owned()
    } else {
        s
    }
}

/// **Overwrite** the behavior on `prim_path` into the current edit layer
/// (Conductor "Overwrite Behavior") — the action offered when a behavior's
/// assignment lives in a *weaker* layer (the local `over` authors nothing for
/// it) or when the `over` has *detached* the `IdtxBehaviorAPI` and only stale
/// weaker-layer `idtx:behavior:*` attributes linger.
///
/// Carries the behavior's **composed** manifest config (kind/scope/enable +
/// authority flags, capabilities, inputs, version, ship, memory cap) into the
/// edit layer, then creates a **fresh** source file with a newly generated uuid
/// (via [`create_behavior_source`], which also re-applies `IdtxBehaviorAPI` into
/// the `over`, overriding a weaker-layer detach). `module`/`source`/`uuid` are
/// *not* copied — the new source replaces them.
///
/// The config snapshot is read **before** the fresh source is authored, so the
/// weaker-layer values win over the defaults `create_behavior_source` seeds
/// (notably `kind = "runtime"`). Returns the fresh source path (for a
/// non-blocking [`start_check`]); errors are human-facing.
#[cfg(feature = "authoring")]
pub(crate) fn overwrite_behavior(
    session: &mut StudioSession,
    prim_path: &str,
) -> Result<String, String> {
    // Capture the inherited/lingering config first — once the fresh source is
    // authored it seeds its own defaults into the edit layer.
    let cfg = session
        .read_composed_behavior_config(prim_path)
        .ok_or_else(|| "Select a prim to overwrite a behavior on".to_owned())?;

    // Fresh source + new uuid; also re-applies IdtxBehaviorAPI into the `over`.
    let source_asset = create_behavior_source(session, prim_path)?;

    // Re-author the captured manifest config as strong opinions in the edit
    // layer (overwrites the `runtime` kind default seeded above).
    session
        .apply_behavior_config(prim_path, &cfg)
        .map_err(|e| format!("Failed to author behavior config: {e}"))?;

    Ok(source_asset)
}

/// The leaf (last `/`-segment) of a prim path, defaulting to `"behavior"`.
#[cfg(feature = "authoring")]
fn prim_leaf(prim_path: &str) -> &str {
    prim_path
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("behavior")
}

/// Sanitise a prim name into a lower-snake behavior file stem (`<name>_behavior`).
#[cfg(feature = "authoring")]
fn sanitize_stem(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    while s.contains("__") {
        s = s.replace("__", "_");
    }
    let s = s.trim_matches('_').to_owned();
    if s.is_empty() {
        "behavior".to_owned()
    } else {
        format!("{s}_behavior")
    }
}

/// Return a stem under `dir` that does not collide with a source folder or flat entry,
/// suffixing `_2`, `_3`, … as needed.
#[cfg(feature = "authoring")]
fn unique_stem(dir: &Path, base: &str) -> String {
    for n in 1_u64.. {
        let candidate = if n == 1 { base.to_owned() } else { format!("{base}_{n}") };
        if !dir.join(&candidate).exists() && !dir.join(format!("{candidate}.rs")).exists() {
            return candidate;
        }
    }
    unreachable!("source name space exhausted")
}
/// `prim_path`. The result is published to the inspector by [`poll_checks`]
/// once the worker finishes; on an immediate resolution failure the panel is
/// flipped straight to [`BehaviorCheckState::Failed`].
pub(crate) fn start_check(egui_ctx: &egui::Context, session: &mut StudioSession, prim_path: &str) {
    let Some(src) = resolve_behavior_source(session, prim_path) else {
        set_behavior_check(
            egui_ctx,
            prim_path,
            BehaviorCheckState::Failed("No behavior source could be resolved".to_owned()),
        );
        return;
    };
    if !src.is_file() {
        set_behavior_check(
            egui_ctx,
            prim_path,
            BehaviorCheckState::Failed(format!("source not found: {}", src.display())),
        );
        return;
    }
    let Some(sdk_path) = discover_sdk_path() else {
        set_behavior_check(
            egui_ctx,
            prim_path,
            BehaviorCheckState::Failed(
                "Could not locate the idtx-behavior-sdk crate (set IDTX_BEHAVIOR_SDK)".to_owned(),
            ),
        );
        return;
    };

    let out_dir = authoring_root(session).join(".idtx/behavior-build");
    let opts = BuildOptions::new(out_dir, sdk_path);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = check_behavior(&src, &opts).map_err(|e| e.to_string());
        let _ = tx.send(result);
    });
    set_behavior_check(egui_ctx, prim_path, BehaviorCheckState::Running);
    CHECK_JOBS
        .lock()
        .expect("behavior check-jobs poisoned")
        .push(CheckJob {
            target_path: prim_path.to_owned(),
            rx,
        });
}

/// Drain finished compile-check jobs and publish their results to the
/// inspector panel. Called once per frame from the shell's render loop.
pub(crate) fn poll_checks(egui_ctx: &egui::Context) {
    let mut jobs = CHECK_JOBS.lock().expect("behavior check-jobs poisoned");
    if jobs.is_empty() {
        return;
    }
    jobs.retain(|job| match job.rx.try_recv() {
        Ok(result) => {
            let state = match result {
                Ok(()) => BehaviorCheckState::Passed,
                Err(msg) => BehaviorCheckState::Failed(msg),
            };
            set_behavior_check(egui_ctx, &job.target_path, state);
            egui_ctx.request_repaint();
            false
        }
        Err(TryRecvError::Empty) => true,
        Err(TryRecvError::Disconnected) => {
            set_behavior_check(
                egui_ctx,
                &job.target_path,
                BehaviorCheckState::Failed("check worker disconnected".to_owned()),
            );
            false
        }
    });
}

/// Kick a background **build + link-verification** for the behavior on
/// `prim_path`.
///
/// Unlike [`start_check`] (which runs only `cargo check` and produces no
/// artifact), this runs a full [`build_behavior`] to obtain a real `.wasm`, then
/// reproduces the runtime link gate (`idtx_behavior::verify_link`) against the
/// capabilities authored on the prim — reporting the required / missing / unused
/// buckets. The result is published to the inspector by [`poll_verifies`]. Never
/// runs the behavior.
pub(crate) fn start_verify(
    egui_ctx: &egui::Context,
    session: &mut StudioSession,
    prim_path: &str,
) {
    let Some(src) = resolve_behavior_source(session, prim_path) else {
        set_behavior_verify(
            egui_ctx,
            prim_path,
            BehaviorVerifyState::Failed("No behavior source could be resolved".to_owned()),
        );
        return;
    };
    if !src.is_file() {
        set_behavior_verify(
            egui_ctx,
            prim_path,
            BehaviorVerifyState::Failed(format!("source not found: {}", src.display())),
        );
        return;
    }
    let Some(sdk_path) = discover_sdk_path() else {
        set_behavior_verify(
            egui_ctx,
            prim_path,
            BehaviorVerifyState::Failed(
                "Could not locate the idtx-behavior-sdk crate (set IDTX_BEHAVIOR_SDK)".to_owned(),
            ),
        );
        return;
    };

    // The capability grant authored on the prim, to check the code against.
    // Empty when nothing is authored (⇒ the verifier reports every used bucket
    // as missing, which is exactly the "granted nothing" story).
    let configured = session
        .read_composed_behavior_config(prim_path)
        .map(|c| c.capabilities)
        .unwrap_or_default();

    let out_dir = authoring_root(session).join(".idtx/behavior-build");
    let opts = BuildOptions::new(out_dir, sdk_path);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(run_verify(&src, &opts, &configured));
    });
    set_behavior_verify(egui_ctx, prim_path, BehaviorVerifyState::Running);
    VERIFY_JOBS
        .lock()
        .expect("behavior verify-jobs poisoned")
        .push(VerifyJob {
            target_path: prim_path.to_owned(),
            rx,
        });
}

/// Build the behavior and link-verify the produced `.wasm` against the authored
/// capability tokens. Returns a [`LinkSummary`] (token lists for the inspector)
/// or a human-facing error. Runs on the worker thread.
fn run_verify(src: &Path, opts: &BuildOptions, configured: &[String]) -> Result<LinkSummary, String> {
    let artifact = build_behavior(src, opts).map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&artifact.wasm_path).map_err(|e| {
        format!(
            "could not read built module {}: {e}",
            artifact.wasm_path.display()
        )
    })?;
    let caps = Capabilities::from_tokens(configured.iter().map(String::as_str));
    let report = verify_link(&bytes, caps).map_err(|e| e.to_string())?;
    let tokens = |c: Capabilities| -> Vec<String> {
        c.to_tokens().into_iter().map(str::to_owned).collect()
    };
    Ok(LinkSummary {
        links: report.links,
        required: tokens(report.required),
        missing: tokens(report.missing),
        unused: tokens(report.unused),
    })
}

/// Drain finished verify jobs and publish their results to the inspector panel.
/// Called once per frame from the shell's render loop (sibling of
/// [`poll_checks`]).
pub(crate) fn poll_verifies(egui_ctx: &egui::Context) {
    let mut jobs = VERIFY_JOBS.lock().expect("behavior verify-jobs poisoned");
    if jobs.is_empty() {
        return;
    }
    jobs.retain(|job| match job.rx.try_recv() {
        Ok(result) => {
            let state = match result {
                Ok(summary) => BehaviorVerifyState::Ok(summary),
                Err(msg) => BehaviorVerifyState::Failed(msg),
            };
            set_behavior_verify(egui_ctx, &job.target_path, state);
            egui_ctx.request_repaint();
            false
        }
        Err(TryRecvError::Empty) => true,
        Err(TryRecvError::Disconnected) => {
            set_behavior_verify(
                egui_ctx,
                &job.target_path,
                BehaviorVerifyState::Failed("verify worker disconnected".to_owned()),
            );
            false
        }
    });
}

/// The directory of the currently-edited stage — the authoring layer against
/// which a stage-relative `idtx:behavior:source` (and the `behaviors/` folder
/// Create writes into) is anchored. Defaults to `.` for an unnamed stage.
///
/// The session's `source_path()` is derived from a canonicalized project root,
/// which on Windows carries a verbatim (`\\?\`) prefix; strip it here so every
/// stage-relative fallback / `.wasm` path this panel produces (and displays) is
/// a plain, cross-platform value.
fn stage_dir(session: &StudioSession) -> PathBuf {
    let dir = session
        .source_path()
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    idtx_project::strip_verbatim_prefix(&dir)
}

pub(crate) fn authoring_root(session: &StudioSession) -> PathBuf {
    let stage = stage_dir(session);
    stage.ancestors().find(|p| p.join(idtx_project::PROJECT_MANIFEST_FILENAME).is_file())
        .unwrap_or(&stage).to_path_buf()
}

/// Resolve the Rust source file for the behavior on `prim_path`.
///
/// Source-first: an explicitly-authored `idtx:behavior:source` (`.rs`) wins — the
/// shape a Conductor-created, source-only behavior carries before any `.wasm`
/// exists. It resolves relative to the **stage directory** (the layer that
/// authored the opinion), matching where Create writes it and where USD resolves
/// it in preview/packaging. Failing that, falls back to the `idtx:behavior:module`
/// `.wasm` attachment's sibling `.rs` (same stem, also stage-relative); when
/// that is absent a `behaviors/<stem>.rs` under the stage directory is tried
/// before giving up (so a not-yet-built behavior whose source lives in
/// `behaviors/` still resolves). Returns the preferred candidate even when it
/// does not exist so callers can surface a precise "not found" path.
fn resolve_behavior_source(session: &mut StudioSession, prim_path: &str) -> Option<PathBuf> {
    let stage_dir = stage_dir(session);

    // Prefer an explicitly-authored `idtx:behavior:source` (`.rs`) — the shape a
    // Conductor-created, source-only behavior carries before any `.wasm` exists.
    // Resolve it through USD's asset resolver — the SAME resolution
    // preview/packaging use (`resolve_prim_asset`), which anchors the authored
    // path against its authoring layer AND the project's resolver search path.
    // A search-path-anchored `@assets/behaviors/x.rs@` therefore resolves to
    // `<project>/assets/behaviors/x.rs`, matching Check/Verify to preview —
    // instead of the old naive `stage_dir.join(..)` that produced the wrong
    // `.../assets/widgets/assets/behaviors/x.rs`.
    if let Some(src_rel) = session.behavior_source_path(prim_path) {
        if let Some(resolved) = session.resolve_behavior_source_path(prim_path) {
            return Some(resolved);
        }
        // Resolver yielded nothing (prim/attr vanished, or the authored value is
        // not anchorable) — fall back to the legacy stage-relative join so a
        // concrete candidate path is still reported in the "not found" message.
        return Some(resolve_relative(&stage_dir, &src_rel));
    }

    // Fall back to the `.wasm`-attachment → sibling-`.rs` convention (a behavior
    // attached from a pre-built module, or a legacy stage). This attachment is
    // authored relative to the composed stage, so it stays stage-anchored.
    let wasm_rel = attached_module_path(session, prim_path)?;
    let wasm_abs = resolve_relative(&stage_dir, &wasm_rel);
    let sibling_rs = wasm_abs.with_extension("rs");
    if sibling_rs.is_file() {
        return Some(sibling_rs);
    }

    if let Some(stem) = wasm_abs.file_stem().and_then(|s| s.to_str()) {
        let behaviors_rs = stage_dir.join("behaviors").join(format!("{stem}.rs"));
        if behaviors_rs.is_file() {
            return Some(behaviors_rs);
        }
    }

    // Neither exists yet — return the sibling candidate so the caller reports a
    // concrete path.
    Some(sibling_rs)
}

/// Read the authored `idtx:behavior:module` `.wasm` asset path for `prim_path`, or
/// `None` when no (non-empty) attachment is present.
fn attached_module_path(session: &mut StudioSession, prim_path: &str) -> Option<String> {
    let props = session.inspect_prim(prim_path).ok()?;
    let rec = props
        .into_iter()
        .find(|p| p.name == BEHAVIOR_ATTACHMENT && p.authored)?;
    asset_path_from_summary(&rec.value_summary)
}

/// Recover the asset path from USD's `@<path>@`-delimited `SdfAssetPath`
/// summary; `None` when empty (the "empty asset ⇒ detached" convention).
pub(crate) fn asset_path_from_summary(summary: &str) -> Option<String> {
    let s = summary.trim();
    let path = match (s.find('@'), s.rfind('@')) {
        (Some(first), Some(last)) if last > first => &s[first + 1..last],
        _ => s,
    }
    .trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_owned())
    }
}

/// Resolve `rel` (possibly `./`-prefixed or absolute) against `base`.
pub(crate) fn resolve_relative(base: &Path, rel: &str) -> PathBuf {
    let trimmed = rel.trim_start_matches("./");
    let p = Path::new(trimmed);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

/// Locate the `idtx-behavior-sdk` crate (the compile skeleton's pinned SDK
/// dependency). Order: `IDTX_BEHAVIOR_SDK` env, then walk up from the current
/// exe directory and the working directory looking for a dev-checkout
/// `crates/idtx-behavior-sdk/Cargo.toml`, then a **staged sysroot** shipped next
/// to a packaged executable (`resolve_sdk_dir`) so a bundled Conductor with no
/// repo present can still compile behaviors.
pub(crate) fn discover_sdk_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("IDTX_BEHAVIOR_SDK") {
        return Some(PathBuf::from(dir));
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    for root in roots {
        let mut cur: Option<&Path> = Some(root.as_path());
        while let Some(dir) = cur {
            let candidate = dir.join("crates").join("idtx-behavior-sdk");
            if candidate.join("Cargo.toml").is_file() {
                return Some(candidate);
            }
            cur = dir.parent();
        }
    }
    // Packaged layout: a staged SDK sysroot beside the executable (or in a
    // macOS `.app`'s Resources), discovered by the build crate.
    resolve_sdk_dir(std::env::current_exe().ok().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_summary_strips_at_delimiters() {
        assert_eq!(
            asset_path_from_summary("@behaviors/spin.wasm@").as_deref(),
            Some("behaviors/spin.wasm")
        );
        assert_eq!(asset_path_from_summary("@@"), None);
        assert_eq!(asset_path_from_summary("  "), None);
    }

    #[test]
    fn resolve_relative_joins_and_passes_absolute() {
        let base = Path::new("/stage/dir");
        assert_eq!(
            resolve_relative(base, "./behaviors/spin.wasm"),
            PathBuf::from("/stage/dir/behaviors/spin.wasm")
        );
        assert_eq!(
            resolve_relative(base, "/abs/spin.wasm"),
            PathBuf::from("/abs/spin.wasm")
        );
    }

    #[cfg(feature = "authoring")]
    #[test]
    fn prim_leaf_takes_last_nonempty_segment() {
        assert_eq!(prim_leaf("/World/Cube"), "Cube");
        assert_eq!(prim_leaf("/World/Group/"), "Group");
        assert_eq!(prim_leaf("/"), "behavior");
        assert_eq!(prim_leaf(""), "behavior");
    }

    #[cfg(feature = "authoring")]
    #[test]
    fn sanitize_stem_lower_snakes_and_suffixes() {
        assert_eq!(sanitize_stem("Cube"), "cube_behavior");
        assert_eq!(sanitize_stem("My Cool-Prim"), "my_cool_prim_behavior");
        assert_eq!(sanitize_stem("__"), "behavior");
        assert_eq!(sanitize_stem("123"), "123_behavior");
    }

    #[cfg(feature = "authoring")]
    #[test]
    fn unique_stem_avoids_existing_files() {
        let dir = std::env::temp_dir().join(format!("idtx-uniqstem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // No collision → base returned as-is.
        assert_eq!(unique_stem(&dir, "cube_behavior"), "cube_behavior");

        // Create the base; the next call must suffix `_2`.
        std::fs::write(dir.join("cube_behavior.rs"), b"").unwrap();
        assert_eq!(unique_stem(&dir, "cube_behavior"), "cube_behavior_2");
        std::fs::create_dir(dir.join("cube_behavior_2")).unwrap();
        assert_eq!(unique_stem(&dir, "cube_behavior"), "cube_behavior_3");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "authoring")]
    #[test]
    fn parse_source_kind_reads_behavior_attr() {
        assert_eq!(
            parse_source_kind("#[behavior(kind = \"editor\")]\nimpl X {}"),
            "editor"
        );
        assert_eq!(
            parse_source_kind("#[behavior(kind = \"runtime\")]\nimpl X {}"),
            "runtime"
        );
        // Whitespace variations still resolve.
        assert_eq!(parse_source_kind("#[behavior(kind=\"editor\")]"), "editor");
        // No attribute → the macro's default kind.
        assert_eq!(parse_source_kind("// just a comment\nimpl X {}"), "runtime");
        // An `editor` mention outside the attribute body must not mislead the scan.
        assert_eq!(
            parse_source_kind("// editor note\n#[behavior(kind = \"runtime\")]"),
            "runtime"
        );
    }

    #[cfg(feature = "authoring")]
    #[test]
    fn sanitize_file_stem_preserves_name_without_suffix() {
        assert_eq!(sanitize_file_stem("grab_prim"), "grab_prim");
        assert_eq!(sanitize_file_stem("My Cool-Behavior"), "my_cool_behavior");
        assert_eq!(sanitize_file_stem("__"), "behavior");
        assert_eq!(sanitize_file_stem(""), "behavior");
    }

    #[cfg(feature = "authoring")]
    #[test]
    fn shared_sources_are_referenced_relative_to_the_authoring_layer() {
        let directory = tempfile::tempdir().unwrap();
        let stage = directory.path().join("levels/main");
        let source = directory.path().join("behaviors/spin/behavior.rs");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "").unwrap();
        let asset = source_asset_path(&stage, &source).unwrap();
        assert_eq!(asset, "./../../behaviors/spin/behavior.rs");
        assert_eq!(std::fs::canonicalize(resolve_relative(&stage, &asset)).unwrap(), std::fs::canonicalize(source).unwrap());
    }
}
