//! Deterministic changes between validation and file creation. The prepared
//! target, rather than a returned `PathBuf`, must carry the write boundary.

use std::path::Path;
use idtx_project::ProjectRoot;

#[test]
fn confined_writes_create_parents_and_create_new_never_truncates() {
    let dir = tempfile::tempdir().unwrap();
    let root = ProjectRoot::from_canonical(std::fs::canonicalize(dir.path()).unwrap()).unwrap();
    let file = root.prepare_file(Path::new("one/two/new.txt")).unwrap();
    file.write_new("first").unwrap();
    assert_eq!(file.read_to_string().unwrap(), "first");
    assert_eq!(file.write_new("clobber").unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(file.read_to_string().unwrap(), "first");
    file.write("replacement").unwrap();
    assert_eq!(file.read_to_string().unwrap(), "replacement");
    assert!(root.prepare_file(Path::new("../outside")).is_err());
    assert!(root.prepare_file(Path::new("")).is_err());
}

#[test]
fn invalid_roots_do_not_fall_back_to_an_unchecked_path() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("absent");
    assert!(ProjectRoot::from_canonical(missing.clone()).is_err());
    assert!(ProjectRoot::resolve_from_cli(Some(&missing)).is_err());
    std::fs::write(dir.path().join("file"), "not a directory").unwrap();
    assert!(ProjectRoot::from_canonical(dir.path().join("file")).is_err());
}

#[cfg(unix)]
mod unix {
    use super::*;
    use idtx_project::{IdtxProject, ProjectManifest};
    use std::os::unix::fs::symlink;

    #[test]
    fn replacing_a_validated_parent_with_an_outward_link_cannot_redirect_a_write() {
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = ProjectRoot::from_canonical(std::fs::canonicalize(inside.path()).unwrap()).unwrap();
        root.create_dir_all(Path::new("parent")).unwrap();
        let target = root.prepare_file(Path::new("parent/new/file.txt")).unwrap();
        std::fs::rename(inside.path().join("parent"), inside.path().join("saved")).unwrap();
        symlink(outside.path(), inside.path().join("parent")).unwrap();
        assert!(target.write_new("must stay inside").is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        assert!(!inside.path().join("saved/new").exists());
    }

    #[test]
    fn replacing_a_validated_leaf_cannot_modify_an_external_file() {
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = ProjectRoot::from_canonical(std::fs::canonicalize(inside.path()).unwrap()).unwrap();
        let target = root.prepare_file(Path::new("file.txt")).unwrap();
        let victim = outside.path().join("victim.txt");
        std::fs::write(&victim, "keep").unwrap();
        symlink(&victim, target.path()).unwrap();
        assert!(target.write("changed").is_err());
        assert!(target.write_new("changed").is_err());
        assert_eq!(std::fs::read_to_string(victim).unwrap(), "keep");
    }

    #[test]
    fn canonical_in_root_alias_can_be_retargeted_without_redirecting_the_prepared_write() {
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = ProjectRoot::from_canonical(std::fs::canonicalize(inside.path()).unwrap()).unwrap();
        root.create_dir_all(Path::new("real")).unwrap();
        symlink(root.as_path().join("real"), root.as_path().join("alias")).unwrap();
        let target = root.prepare_file(Path::new("alias/new.txt")).unwrap();
        std::fs::remove_file(root.as_path().join("alias")).unwrap();
        symlink(outside.path(), root.as_path().join("alias")).unwrap();
        target.write_new("inside").unwrap();
        assert_eq!(std::fs::read_to_string(root.as_path().join("real/new.txt")).unwrap(), "inside");
        assert!(!outside.path().join("new.txt").exists());
    }

    #[test]
    fn a_prepared_target_retains_the_original_root_directory() {
        let dir = tempfile::tempdir().unwrap();
        let inside = dir.path().join("inside");
        let outside = dir.path().join("outside");
        std::fs::create_dir(&inside).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let root = ProjectRoot::from_canonical(std::fs::canonicalize(&inside).unwrap()).unwrap();
        let target = root.prepare_file(Path::new("new.txt")).unwrap();
        let original = dir.path().join("original");
        std::fs::rename(&inside, &original).unwrap();
        symlink(&outside, &inside).unwrap();
        target.write_new("original directory").unwrap();
        assert_eq!(std::fs::read_to_string(original.join("new.txt")).unwrap(), "original directory");
        assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
    }

    #[test]
    fn new_project_creation_refuses_an_existing_or_dangling_entry() {
        let parent = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let missing = outside.path().join("missing");
        symlink(&missing, parent.path().join("name")).unwrap();
        assert!(IdtxProject::create_child(parent.path(), Path::new("name"), ProjectManifest::new("new")).is_err());
        assert!(!missing.exists());
        assert!(IdtxProject::create_child(parent.path(), Path::new("../escape"), ProjectManifest::new("new")).is_err());
    }
}
