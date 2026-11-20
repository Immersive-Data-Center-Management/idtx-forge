//! "Create Player Stage" authoring (Conductor DCC surface).
//!
//! Produces a single standalone **player stage** for the open project and
//! records it as the project's [`player_stage`]. The stage is rooted at a
//! `def Xform "Player"` (`defaultPrim = "Player"`) that references the toolbox's
//! **built-in** player (`@built-in/players/Player.usd@</Player>`), so it opens
//! and renders in the editor for customization (add meshes / opinions to the
//! rig) while the cook/preview assembly composer references it into
//! `/World/Player` at build time.
//!
//! The built-in player ships its desktop + XR rigs (chosen by a `playerKind`
//! variantSet) and the default gameplay behaviors as source, so the created
//! stage needs no rig, behavior, controller-mesh, or input-map boilerplate — a
//! referencing project is interactive out of the box. A project only authors
//! local opinions on top when it wants to customize.
//!
//! [`player_stage`]: idtx_project::ProjectManifest::player_stage

use std::path::{Path, PathBuf};

use idtx_project::IdtxProject;

/// The project-relative path of the created player stage.
const PLAYER_STAGE_REL: &str = "stages/player.usda";

/// The result of authoring a player stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerStageArtifacts {
    /// Absolute path of the created player stage (`stageKind = "player"`), opened
    /// for editing after creation.
    pub overlay_path: PathBuf,
}

/// Failure authoring a player stage.
#[derive(Debug, thiserror::Error)]
pub enum PlayerTemplateError {
    /// Writing the stage file (or creating its parent directory) failed.
    #[error("failed to write {path}: {source}")]
    Write {
        /// The file (or directory) that could not be written.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// Recording the stage in the project manifest failed (path escaped the
    /// project root).
    #[error("player stage path escapes the project root")]
    SandboxEscape,
    /// Persisting the updated project manifest failed.
    #[error("failed to save project manifest: {0}")]
    Manifest(#[from] idtx_project::ProjectError),
}

/// The player-stage layer text: a standalone stage whose `defaultPrim` `Player`
/// references the built-in player's `</Player>` prim.
fn player_stage_text() -> String {
    format!(
        "#usda 1.0\n\
         (\n\
         \x20   customLayerData = {{\n\
         \x20       dictionary idtx = {{\n\
         \x20           string displayName = \"Player\"\n\
         \x20           string schemaVersion = \"1\"\n\
         \x20           token stageKind = \"player\"\n\
         \x20       }}\n\
         \x20   }}\n\
         \x20   defaultPrim = \"Player\"\n\
         \x20   metersPerUnit = 1\n\
         \x20   upAxis = \"Y\"\n\
         )\n\
         \n\
         def Xform \"Player\" (\n\
         \x20   prepend references = @{arc}@</Player>\n\
         )\n\
         {{\n\
         }}\n",
        arc = idtx_assets::BUILTIN_PLAYER_STAGE_REF,
    )
}

/// Author the project's player stage at `stages/player.usda` (referencing the
/// built-in player) and record it as the project's `player_stage`.
///
/// Re-running overwrites the stage in place and re-points `player_stage` at it.
/// The project manifest is saved before returning.
///
/// # Errors
///
/// Returns a [`PlayerTemplateError`] when the stage file cannot be written, the
/// path escapes the project root, or the manifest cannot be saved.
pub fn create_player_stage(
    project: &mut IdtxProject,
) -> Result<PlayerStageArtifacts, PlayerTemplateError> {
    let target = project.project_root().prepare_file(Path::new(PLAYER_STAGE_REL))
        .map_err(|source| PlayerTemplateError::Write {
            path: project.root().join(PLAYER_STAGE_REL), source,
        })?;
    let overlay_path = target.path().to_path_buf();
    target.write(player_stage_text()).map_err(|source| {
        PlayerTemplateError::Write {
            path: overlay_path.clone(),
            source,
        }
    })?;

    if !project.set_player_stage(PLAYER_STAGE_REL) {
        return Err(PlayerTemplateError::SandboxEscape);
    }
    project.save()?;

    Ok(PlayerStageArtifacts { overlay_path })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "idtx-playertmpl-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn create_player_stage_writes_stage_and_records_manifest() {
        let dir = scratch("create");
        let mut project =
            IdtxProject::create(&dir, idtx_project::ProjectManifest::new("P")).unwrap();

        let art = create_player_stage(&mut project).unwrap();

        // The stage exists, is tagged, and references the built-in player.
        assert!(art.overlay_path.is_file());
        let text = std::fs::read_to_string(&art.overlay_path).unwrap();
        assert!(text.contains("stageKind = \"player\""), "{text}");
        assert!(text.contains("defaultPrim = \"Player\""), "{text}");
        assert!(
            text.contains("references = @built-in/players/Player.usd@</Player>"),
            "{text}"
        );

        // The manifest now points at the created stage.
        assert_eq!(
            project.manifest().player_stage.as_deref(),
            Some(Path::new("stages/player.usda"))
        );
        assert_eq!(project.player_stage_abs(), Some(art.overlay_path.clone()));

        // Re-running overwrites in place (idempotent — no second stage).
        let art2 = create_player_stage(&mut project).unwrap();
        assert_eq!(art2.overlay_path, art.overlay_path);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn created_stage_persists_across_reload() {
        let dir = scratch("reload");
        let mut project =
            IdtxProject::create(&dir, idtx_project::ProjectManifest::new("P")).unwrap();
        create_player_stage(&mut project).unwrap();

        let reloaded = IdtxProject::open_dir(&dir).unwrap().expect("project");
        assert_eq!(
            reloaded.manifest().player_stage.as_deref(),
            Some(Path::new("stages/player.usda"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn player_creation_rejects_an_escaping_stages_directory_before_writing() {
        let inside = scratch("confined");
        let outside = scratch("outside");
        let mut project = IdtxProject::create(&inside, idtx_project::ProjectManifest::new("P")).unwrap();
        std::os::unix::fs::symlink(&outside, project.root().join("stages")).unwrap();
        let result = create_player_stage(&mut project);
        let escaped = outside.join("player.usda").exists();
        let _ = std::fs::remove_dir_all(&inside);
        let _ = std::fs::remove_dir_all(&outside);
        assert!(result.is_err());
        assert!(!escaped, "player factory wrote outside the project before rejecting the path");
        assert!(project.manifest().player_stage.is_none());
    }
}
