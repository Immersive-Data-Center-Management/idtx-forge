//! Creation-time regressions against the public project boundary.

use std::path::Path;
use idtx_project::ProjectRoot;

#[test]
fn a_non_directory_ancestor_is_not_a_missing_save_target() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("file"), "keep").unwrap();
    let root = ProjectRoot::from_canonical(std::fs::canonicalize(dir.path()).unwrap()).unwrap();
    assert!(root.resolve_relative(Path::new("file/child.usda")).is_none());
    assert!(root.resolve_relative(Path::new("bad\0name.usda")).is_none());
}

#[cfg(unix)]
mod unix {
    use super::*;
    use idtx_project::{IdtxProject, ProjectManifest};
    use std::os::unix::fs::symlink;

    #[test]
    fn missing_targets_below_an_escaping_ancestor_are_rejected() {
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), inside.path().join("escape")).unwrap();
        let root = ProjectRoot::from_canonical(std::fs::canonicalize(inside.path()).unwrap()).unwrap();
        for rel in ["escape/new.usda", "escape/new/deep/file.usda"] {
            assert!(root.resolve_relative(Path::new(rel)).is_none(), "accepted {rel}");
        }
        std::fs::write(outside.path().join("existing.usda"), "outside").unwrap();
        assert!(root.resolve_relative(Path::new("escape/existing.usda")).is_none());
        assert!(root.resolve_relative(Path::new("../outside.usda")).is_none());
        assert!(root.resolve_relative(outside.path()).is_none());
    }

    #[test]
    fn dangling_and_looping_symlinks_are_not_absent_components() {
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path().join("not-created"), inside.path().join("dangling")).unwrap();
        symlink("loop", inside.path().join("loop")).unwrap();
        let root = ProjectRoot::from_canonical(std::fs::canonicalize(inside.path()).unwrap()).unwrap();
        for rel in ["dangling", "dangling/new.usda", "loop", "loop/new.usda"] {
            assert!(root.resolve_relative(Path::new(rel)).is_none(), "accepted {rel}");
        }
        assert!(!outside.path().join("not-created").exists());
    }

    #[test]
    fn in_root_aliases_resolve_to_the_existing_ancestor_before_creation() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(canonical.join("real")).unwrap();
        symlink(canonical.join("real"), canonical.join("alias")).unwrap();
        let root = ProjectRoot::from_canonical(canonical.clone()).unwrap();
        let path = root.resolve_relative(Path::new("alias/new/deep/file.usda")).unwrap();
        assert_eq!(path, canonical.join("real/new/deep/file.usda"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "inside").unwrap();
        assert_eq!(root.resolve_relative(Path::new("alias/new/deep/file.usda")), Some(path));
    }

    #[test]
    fn project_manifest_save_does_not_follow_a_replaced_leaf_outside() {
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let project = IdtxProject::create(inside.path(), ProjectManifest::new("inside")).unwrap();
        let victim = outside.path().join("victim.toml");
        std::fs::write(&victim, "keep").unwrap();
        std::fs::remove_file(project.manifest_path()).unwrap();
        symlink(&victim, project.manifest_path()).unwrap();
        assert!(project.save().is_err());
        assert_eq!(std::fs::read_to_string(victim).unwrap(), "keep");
    }

    #[test]
    fn absent_sidecar_cannot_be_created_through_a_dangling_link() {
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let project = IdtxProject::create(inside.path(), ProjectManifest::new("inside")).unwrap();
        let victim = outside.path().join("new.toml");
        symlink(&victim, project.xr_controllers_path()).unwrap();
        assert!(project.write_default_xr_controllers().is_err());
        assert!(!victim.exists());
    }
}
