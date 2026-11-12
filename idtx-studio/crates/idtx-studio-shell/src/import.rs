//! Conductor "Import" flow: bring an external USD stage into the active
//! project's `assets/` folder before opening it.
//!
//! Unlike the single-stage products' "Open…", Conductor imports so every stage
//! a project references lives inside the project (references resolve, and the
//! project can be packaged as a unit). Three cases, decided by
//! [`classify_import`]:
//!
//! 1. Already under `assets/` → open in place, no copy.
//! 2. A `.usdz` elsewhere → copy the file into `assets/`, then open.
//! 3. A loose `.usd`/`.usdc`/`.usda` elsewhere → package (with all referenced
//!    assets) in private scratch storage, then copy the completed `.usdz` through
//!    the project's confined file capability into `assets/` and open it.
//!
//! Case 3 is a multi-second blocking `OpenUSD` call, so it runs on a worker
//! thread ([`ImportJob`]) polled per frame, behind a cancellable progress
//! modal that surfaces packaging diagnostics for accept/reject.

use std::path::{Path, PathBuf};
use std::io;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use idtx_usd::PackageResult;

/// What to do with a file the user picked to import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportPlan {
    /// The file is already under the project's `assets/` tree — open it
    /// directly, no copy or packaging (spec case 1).
    OpenInPlace,
    /// A `.usdz` outside `assets/` — copy the file into `assets/`, then open
    /// (spec case 2).
    CopyUsdz,
    /// A loose `.usd`/`.usdc`/`.usda` outside `assets/` — package into a
    /// `.usdz` inside `assets/`, then open (spec case 3).
    PackageLoose,
}

/// Classify a picked file for import.
///
/// `picked` and `assets_dir` should already be canonicalized (or at least both
/// absolute) so the containment check is reliable. Extension matching is
/// case-insensitive. A file already under `assets/` opens in place regardless
/// of format; otherwise a `.usdz` is copied and anything else is packaged.
pub(crate) fn classify_import(picked: &Path, assets_dir: &Path) -> ImportPlan {
    if picked.starts_with(assets_dir) {
        return ImportPlan::OpenInPlace;
    }
    let ext = picked
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("usdz") => ImportPlan::CopyUsdz,
        _ => ImportPlan::PackageLoose,
    }
}

/// Choose a non-clobbering path `<assets_dir>/<stem>.<ext>`, appending `-1`,
/// `-2`, … before the extension until a free name is found. Deterministic given
/// the current directory contents.
pub(crate) fn dedup_dest(assets_dir: &Path, stem: &str, ext: &str) -> io::Result<PathBuf> {
    for n in 0..=u32::MAX {
        let name = if n == 0 { format!("{stem}.{ext}") } else { format!("{stem}-{n}.{ext}") };
        let candidate = assets_dir.join(name);
        match std::fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(candidate),
            Ok(_) => {} // A dangling link also occupies a name.
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "no unused import name available"))
}

/// Validate the destination now and retain a root handle for the eventual copy.
/// An actual create-new open at completion also prevents collision races.
pub(crate) fn prepare_import(root: &idtx_project::ProjectRoot, source: &Path) -> io::Result<idtx_project::ProjectFileTarget> {
    let assets = root.create_dir_all(Path::new("assets"))?;
    let stem = source.file_stem().and_then(|s| s.to_str()).unwrap_or("imported");
    let path = dedup_dest(&assets, stem, "usdz")?;
    root.prepare_file(&root.relative_path(&path)?)
}

pub(crate) fn copy_into_project(root: &idtx_project::ProjectRoot, source: &Path) -> io::Result<PathBuf> {
    let target = prepare_import(root, source)?;
    target.copy_new_from(source)?;
    Ok(target.path().to_path_buf())
}

/// `OpenUSD` takes a path rather than a directory handle, so it only receives a
/// host-owned scratch location. Workers retain an `Arc` until packaging returns.
pub(crate) fn import_scratch() -> io::Result<Arc<tempfile::TempDir>> {
    tempfile::Builder::new().prefix("idtx-import-").tempdir().map(Arc::new)
}

/// Where a background import currently is.
#[derive(Clone)]
pub(crate) enum ImportPhase {
    /// Packaging in progress — show a spinner + Cancel.
    Packaging,
    /// Packaging found references that can't be resolved. Awaiting the user's
    /// decision: Cancel (default) or "Import anyway", which re-packages with the
    /// broken references stripped out. Carries the unresolved asset paths to
    /// show which assets are missing.
    AwaitingUnresolved(Vec<String>),
    /// Packaging finished but `OpenUSD` emitted diagnostics; awaiting the user's
    /// accept ("Import anyway") / reject ("Cancel") decision.
    AwaitingWarnings(Vec<String>),
    /// Packaging failed with a human-facing message.
    Failed(String),
}

/// A running (case-3) packaging import, owned by the shell and polled per frame.
pub(crate) struct ImportJob {
    /// Worker result: the [`PackageResult`] (carrying any diagnostics) or an
    /// error message.
    pub(crate) rx: Receiver<Result<PackageResult, String>>,
    /// The external source file being imported. Kept so an "Import anyway"
    /// decision can re-spawn the (ref-stripping) packaging worker.
    pub(crate) source: PathBuf,
    /// Private scratch storage, retained by both the job and its worker.
    pub(crate) scratch: Arc<tempfile::TempDir>,
    /// The private file the path-only USD packager writes.
    pub(crate) temp_dest: PathBuf,
    /// The destination capability, captured from the project that started import.
    pub(crate) target: idtx_project::ProjectFileTarget,
    /// Source file name, for the modal label.
    pub(crate) source_label: String,
    /// Current phase, drives the modal.
    pub(crate) phase: ImportPhase,
    /// The user cancelled while packaging: the modal is hidden immediately and
    /// the result is discarded (temp deleted) when the un-interruptible worker
    /// eventually finishes.
    pub(crate) abandoned: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_assets_opens_in_place() {
        let assets = Path::new("/proj/assets");
        assert_eq!(
            classify_import(Path::new("/proj/assets/sub/mesh.usdc"), assets),
            ImportPlan::OpenInPlace
        );
        // Even a loose format opens in place when already under assets/.
        assert_eq!(
            classify_import(Path::new("/proj/assets/scene.usda"), assets),
            ImportPlan::OpenInPlace
        );
    }

    #[test]
    fn external_usdz_is_copied() {
        assert_eq!(
            classify_import(
                Path::new("/downloads/model.usdz"),
                Path::new("/proj/assets")
            ),
            ImportPlan::CopyUsdz
        );
        // Case-insensitive extension.
        assert_eq!(
            classify_import(
                Path::new("/downloads/model.USDZ"),
                Path::new("/proj/assets")
            ),
            ImportPlan::CopyUsdz
        );
    }

    #[test]
    fn external_loose_is_packaged() {
        for ext in ["usd", "usda", "usdc"] {
            assert_eq!(
                classify_import(
                    &PathBuf::from(format!("/elsewhere/scene.{ext}")),
                    Path::new("/proj/assets")
                ),
                ImportPlan::PackageLoose,
                "{ext} outside assets should be packaged"
            );
        }
    }

    #[test]
    fn sibling_prefix_is_not_treated_as_under_assets() {
        // `/proj/assets-backup` must not count as inside `/proj/assets`.
        assert_ne!(
            classify_import(
                Path::new("/proj/assets-backup/x.usdz"),
                Path::new("/proj/assets")
            ),
            ImportPlan::OpenInPlace
        );
    }

    #[test]
    fn dedup_returns_bare_name_when_free() {
        let dir = std::env::temp_dir().join(format!("idtx-import-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let got = dedup_dest(&dir, "unique-stem-xyz", "usdz").unwrap();
        assert_eq!(got, dir.join("unique-stem-xyz.usdz"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dedup_appends_suffix_on_collision() {
        let dir = std::env::temp_dir().join(format!("idtx-import-dedup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("thing.usdz"), b"x").unwrap();
        assert_eq!(dedup_dest(&dir, "thing", "usdz").unwrap(), dir.join("thing-1.usdz"));
        std::fs::write(dir.join("thing-1.usdz"), b"x").unwrap();
        assert_eq!(dedup_dest(&dir, "thing", "usdz").unwrap(), dir.join("thing-2.usdz"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_name_taken_during_packaging_is_not_overwritten_on_accept() {
        let dir = tempfile::tempdir().unwrap();
        let root = idtx_project::ProjectRoot::from_canonical(std::fs::canonicalize(dir.path()).unwrap()).unwrap();
        let target = prepare_import(&root, Path::new("model.usda")).unwrap();
        let scratch = import_scratch().unwrap();
        let packaged = scratch.path().join("stage.usdz");
        std::fs::write(&packaged, "packaged").unwrap();
        std::fs::write(target.path(), "other import").unwrap();
        assert_eq!(target.copy_new_from(&packaged).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(target.path()).unwrap(), "other import");
    }

    #[test]
    fn real_package_opens_after_private_staging_and_confined_publication() {
        let dir = tempfile::tempdir().unwrap();
        let root = idtx_project::ProjectRoot::from_canonical(std::fs::canonicalize(dir.path()).unwrap()).unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testfiles/usd/project_confinement/entry.usda");
        let target = prepare_import(&root, &source).unwrap();
        let scratch = import_scratch().unwrap();
        let staged = scratch.path().join("stage.usdz");
        let result = idtx_usd::package_stage(&source, &staged).unwrap();
        assert!(!result.destination.as_os_str().is_empty());
        assert!(result.unresolved.is_empty());
        assert!(!target.path().exists(), "packaging must not write into the project");
        target.copy_new_from(&staged).unwrap();
        drop(scratch);
        let stage = idtx_usd::StageService::open_topology_only(target.path()).unwrap();
        assert!(stage.topology().iter().any(|prim| prim.name == "ImportedChild"));
    }

    #[cfg(unix)]
    #[test]
    fn an_import_cannot_follow_assets_replaced_while_packaging() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = idtx_project::ProjectRoot::from_canonical(std::fs::canonicalize(dir.path()).unwrap()).unwrap();
        let target = prepare_import(&root, Path::new("model.usda")).unwrap();
        let scratch = import_scratch().unwrap();
        let packaged = scratch.path().join("stage.usdz");
        std::fs::write(&packaged, "packaged").unwrap();
        assert_eq!(std::fs::read_dir(dir.path().join("assets")).unwrap().count(), 0);
        std::fs::rename(dir.path().join("assets"), dir.path().join("old-assets")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("assets")).unwrap();
        assert!(target.copy_new_from(&packaged).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn copying_an_asset_preserves_dangling_links_and_external_targets() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = idtx_project::ProjectRoot::from_canonical(std::fs::canonicalize(dir.path()).unwrap()).unwrap();
        root.create_dir_all(Path::new("assets")).unwrap();
        let victim = outside.path().join("new.usdz");
        std::os::unix::fs::symlink(&victim, dir.path().join("assets/model.usdz")).unwrap();
        let source = outside.path().join("model.usdz");
        std::fs::write(&source, "asset").unwrap();
        let copied = copy_into_project(&root, &source).unwrap();
        assert_eq!(copied, root.as_path().join("assets/model-1.usdz"));
        assert_eq!(std::fs::read_to_string(copied).unwrap(), "asset");
        assert!(!victim.exists());
    }
}
