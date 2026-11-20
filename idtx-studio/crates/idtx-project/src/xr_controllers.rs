//! `xr_controllers.toml` — the project-level XR controller device→asset sidecar.
//!
//! Per the conductor design (decision D1) stage-independent config lives in TOML
//! sidecars, not USD metadata. This is the XR analogue of the run-time manifest's
//! `[xr.controllers]` table, authored at the **project root** so the Conductor can
//! edit it independently of any packaged `run.toml`.
//!
//! The concrete controller model is unknown until the headset is on, so the mesh
//! cannot be baked at cook time; instead the packaged runtime resolves
//! device→asset at startup exactly as the preview path does. The cook step
//! folds this sidecar into `run.toml`'s `[xr]` table so the two resolution paths
//! stay identical.
//!
//! ## Format
//!
//! Structurally identical to the manifest's [`XrConfig`] — a `[controllers.<device>]`
//! table per device id, each carrying `left`/`right` (per-hand meshes) or `asset`
//! (one mesh for both hands), plus an optional `[controllers.<device>.mount]` TRS
//! offset that seats the vendor mesh in the `OpenXR` grip frame:
//!
//! ```toml
//! # xr_controllers.toml — per-device XR controller meshes for this project.
//! [controllers.generic]
//! left  = "controllers/generic_left.usda"
//! right = "controllers/generic_right.usda"
//!
//! [controllers.questpro]
//! asset = "controllers/questpro.usda"
//! [controllers.questpro.mount]
//! rotate = [180.0, 0.0, 0.0]   # Euler XYZ degrees
//! ```

use std::path::{Path, PathBuf};

use idtx_runtime_manifest::XrConfig;

use crate::{IdtxProject, ProjectError};

/// Canonical file name of the XR-controllers sidecar, at the project root.
pub const XR_CONTROLLERS_FILENAME: &str = "xr_controllers.toml";

/// Default `xr_controllers.toml` written for a new XR player (only when absent —
/// an existing user sidecar is never clobbered). Baked from the built-in idtx-assets
/// so a packaged `idtx-conductor` carries it;
/// it seeds the `generic` fallback plus the `questpro`/`quest2` device tables in
/// the [`XrConfig`] shape [`parse_xr_controllers`] deserializes.
pub const DEFAULT_XR_CONTROLLERS: &str = include_str!("../../idtx-assets/built-in/projects/xr_controllers.toml");

/// Parse an `xr_controllers.toml` document into an [`XrConfig`].
///
/// The document's top-level `[controllers.*]` tables map directly onto
/// [`XrConfig::controllers`] — the same shape the runtime manifest nests under
/// `[xr]` — so a parsed sidecar can be folded into `run.toml` verbatim at cook.
///
/// # Errors
///
/// [`ProjectError::Parse`] (tagged with `path`) when the text is not valid TOML
/// for the [`XrConfig`] schema.
pub fn parse_xr_controllers(text: &str, path: &Path) -> Result<XrConfig, ProjectError> {
    toml::from_str(text).map_err(|source| ProjectError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

impl IdtxProject {
    /// The on-disk path of the XR-controllers sidecar (`<root>/xr_controllers.toml`),
    /// whether or not it exists.
    #[must_use]
    pub fn xr_controllers_path(&self) -> PathBuf {
        self.root().join(XR_CONTROLLERS_FILENAME)
    }

    /// Load the project's XR-controllers sidecar, or `None` when the file is
    /// absent (an XR-less project, or one relying on the manifest defaults).
    ///
    /// # Errors
    ///
    /// [`ProjectError::Io`] if the file exists but cannot be read;
    /// [`ProjectError::Parse`] if it is not valid TOML for the [`XrConfig`] schema.
    pub fn load_xr_controllers(&self) -> Result<Option<XrConfig>, ProjectError> {
        let path = self.xr_controllers_path();
        let target = self.project_root().prepare_file(Path::new(XR_CONTROLLERS_FILENAME))
            .map_err(|source| ProjectError::Io { path: path.clone(), source })?;
        let text = match target.read_to_string() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            result => result,
        }.map_err(|source| ProjectError::Io {
            path: path.clone(),
            source,
        })?;
        parse_xr_controllers(&text, &path).map(Some)
    }

    /// Author the [`DEFAULT_XR_CONTROLLERS`] sidecar at [`xr_controllers_path`]
    /// **only when absent** — a project's existing device→mesh config is never
    /// clobbered, so re-running "New Player Stage… (XR)" leaves a customised
    /// sidecar alone. Mirrors the input-map generator's write-if-absent contract.
    ///
    /// # Errors
    ///
    /// [`ProjectError::Write`] (tagged with the sidecar path) if the file is
    /// absent but cannot be created.
    ///
    /// [`xr_controllers_path`]: IdtxProject::xr_controllers_path
    pub fn write_default_xr_controllers(&self) -> Result<(), ProjectError> {
        let path = self.xr_controllers_path();
        let target = self.project_root().prepare_file(Path::new(XR_CONTROLLERS_FILENAME))
            .map_err(|source| ProjectError::Write { path: path.clone(), source })?;
        match target.write_new(DEFAULT_XR_CONTROLLERS) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            result => result.map_err(|source| ProjectError::Write { path, source }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::float_cmp)] // exact mount literals from parsed TOML, bit-exact
    fn parses_per_hand_and_shared_meshes_with_mount() {
        let text = "\
[controllers.generic]
left  = \"controllers/generic_left.usda\"
right = \"controllers/generic_right.usda\"

[controllers.questpro]
asset = \"controllers/questpro.usda\"
[controllers.questpro.mount]
rotate = [180.0, 0.0, 0.0]
";
        let cfg = parse_xr_controllers(text, Path::new("xr_controllers.toml")).unwrap();
        let generic = cfg.controllers.get("generic").expect("generic device");
        assert_eq!(
            generic.left.as_deref(),
            Some(Path::new("controllers/generic_left.usda"))
        );
        assert_eq!(
            generic.right.as_deref(),
            Some(Path::new("controllers/generic_right.usda"))
        );
        assert!(generic.mount.is_none());

        let quest = cfg.controllers.get("questpro").expect("questpro device");
        assert_eq!(
            quest.asset.as_deref(),
            Some(Path::new("controllers/questpro.usda"))
        );
        let mount = quest.mount.expect("questpro mount");
        assert_eq!(mount.rotate(), [180.0, 0.0, 0.0]);
        // Unset TRS components resolve to their identities.
        assert_eq!(mount.translate(), [0.0, 0.0, 0.0]);
        assert_eq!(mount.scale(), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn empty_document_is_an_empty_config() {
        let cfg = parse_xr_controllers("", Path::new("xr_controllers.toml")).unwrap();
        assert!(cfg.controllers.is_empty());
    }

    #[test]
    fn invalid_toml_is_a_parse_error_tagged_with_path() {
        let err = parse_xr_controllers("controllers = 5", Path::new("bad.toml")).unwrap_err();
        match err {
            ProjectError::Parse { path, .. } => assert_eq!(path, Path::new("bad.toml")),
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn absent_sidecar_loads_as_none() {
        let dir = std::env::temp_dir().join(format!("idtx-xrctrl-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = dir.join("project.toml");
        std::fs::write(&manifest, "version = 1\nname = \"Scratch\"\n").unwrap();
        let project = IdtxProject::load(&manifest).unwrap();
        assert!(project.load_xr_controllers().unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn present_sidecar_loads_and_parses() {
        let dir = std::env::temp_dir().join(format!("idtx-xrctrl-present-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = dir.join("project.toml");
        std::fs::write(&manifest, "version = 1\nname = \"Scratch\"\n").unwrap();
        std::fs::write(
            dir.join(XR_CONTROLLERS_FILENAME),
            "[controllers.generic]\nasset = \"controllers/g.usda\"\n",
        )
        .unwrap();
        let project = IdtxProject::load(&manifest).unwrap();
        let cfg = project
            .load_xr_controllers()
            .unwrap()
            .expect("sidecar present");
        assert!(cfg.controllers.contains_key("generic"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn baked_default_template_round_trips_through_parser() {
        let cfg = parse_xr_controllers(DEFAULT_XR_CONTROLLERS, Path::new(XR_CONTROLLERS_FILENAME))
            .unwrap();
        // The baked template must carry the generic fallback plus the two named
        // devices seeded from run.toml, or the factory ships a stale/broken sidecar.
        assert!(cfg.controllers.contains_key("generic"), "generic fallback");
        assert!(cfg.controllers.contains_key("questpro"), "questpro device");
        assert!(cfg.controllers.contains_key("quest2"), "quest2 device");
        assert!(
            cfg.controllers.get("questpro").unwrap().mount.is_some(),
            "questpro carries a mount TRS"
        );
    }

    #[test]
    fn write_default_authors_when_absent_and_round_trips() {
        let dir =
            std::env::temp_dir().join(format!("idtx-xrctrl-writedefault-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = dir.join("project.toml");
        std::fs::write(&manifest, "version = 1\nname = \"Scratch\"\n").unwrap();
        let project = IdtxProject::load(&manifest).unwrap();

        assert!(project.load_xr_controllers().unwrap().is_none());
        project.write_default_xr_controllers().unwrap();
        assert!(project.xr_controllers_path().is_file());
        // What we just wrote loads back through the reader.
        let cfg = project
            .load_xr_controllers()
            .unwrap()
            .expect("sidecar written");
        assert!(cfg.controllers.contains_key("generic"));
        assert!(cfg.controllers.contains_key("questpro"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_default_never_clobbers_an_existing_sidecar() {
        let dir =
            std::env::temp_dir().join(format!("idtx-xrctrl-noclobber-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = dir.join("project.toml");
        std::fs::write(&manifest, "version = 1\nname = \"Scratch\"\n").unwrap();
        let project = IdtxProject::load(&manifest).unwrap();

        let custom = "[controllers.generic]\nasset = \"controllers/mine.usda\"\n";
        std::fs::write(project.xr_controllers_path(), custom).unwrap();
        project.write_default_xr_controllers().unwrap();
        assert_eq!(
            std::fs::read_to_string(project.xr_controllers_path()).unwrap(),
            custom,
            "an existing sidecar must be left untouched"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
