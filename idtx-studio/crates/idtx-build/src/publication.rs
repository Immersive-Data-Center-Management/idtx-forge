//! Private staging and complete, path-sensitive payload stamps.
use std::{fmt::Write as _, fs, io, path::{Path, PathBuf}};
use sha2::{Digest, Sha256};
use crate::CookError;

fn io_at(path: &Path, source: io::Error) -> CookError {
    CookError::Io { path: path.to_owned(), source }
}

pub(crate) struct Publication {
    scratch: tempfile::TempDir,
    destination: PathBuf,
}

impl Publication {
    pub(crate) fn new(destination: &Path) -> Result<Self, CookError> {
        let parent = destination.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| io_at(parent, e))?;
        let destination = fs::canonicalize(parent).map_err(|e| io_at(parent, e))?
            .join(destination.file_name().ok_or_else(|| io_at(destination, io::Error::other("output needs a directory name")))?);
        if let Ok(meta) = fs::symlink_metadata(&destination)
            && (!meta.is_dir() || meta.is_symlink()) {
            return Err(io_at(&destination, io::Error::other("output must be a real directory")));
        }
        let scratch = tempfile::Builder::new().prefix(".idtx-publish-").tempdir_in(parent)
            .map_err(|e| io_at(parent, e))?;
        fs::create_dir(scratch.path().join("tree")).map_err(|e| io_at(parent, e))?;
        Ok(Self { scratch, destination })
    }

    pub(crate) fn path(&self) -> PathBuf { self.scratch.path().join("tree") }

    pub(crate) fn destination(&self) -> &Path { &self.destination }

    pub(crate) fn publish(self) -> Result<PathBuf, CookError> {
        let tree = self.path();
        if self.destination.exists() {
            if let Err(error) = replace_directory(&tree, &self.destination) {
                if self.scratch.path().join("previous").exists() {
                    let recovery = self.scratch.keep();
                    return Err(io_at(&self.destination, io::Error::other(format!(
                        "{error}; previous output retained at {}", recovery.join("previous").display()))));
                }
                return Err(io_at(&self.destination, error));
            }
        } else {
            fs::rename(&tree, &self.destination).map_err(|e| io_at(&self.destination, e))?;
        }
        Ok(self.destination.clone())
    }
}

// Exchange keeps readers on either complete generation, with no missing-path
// window. On failure neither directory changes; TempDir removes only scratch.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn replace_directory(from: &Path, to: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both NUL-terminated strings live across the syscall. No pointers
    // are retained. Exchange acts on directory entries, not their children.
    let result = unsafe {
        #[cfg(target_os = "macos")]
        { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) }
        #[cfg(target_os = "linux")]
        { libc::renameat2(libc::AT_FDCWD, from.as_ptr(), libc::AT_FDCWD, to.as_ptr(), libc::RENAME_EXCHANGE) }
    };
    if result == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

// These hosts lack directory exchange. Never remove a previously good tree:
// retain a recovery copy and restore it if the final rename fails.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn replace_directory(from: &Path, to: &Path) -> io::Result<()> {
    replace_with_rollback(from, to, |from, to| fs::rename(from, to))
}

#[cfg(any(test, not(any(target_os = "macos", target_os = "linux"))))]
fn replace_with_rollback(from: &Path, to: &Path, mut rename: impl FnMut(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    let backup = from.with_file_name("previous");
    rename(to, &backup)?;
    if let Err(error) = rename(from, to) {
        if let Err(restore) = rename(&backup, to) {
            // Publication retains the scratch owner if this backup remains.
            return Err(io::Error::other(format!("{error}; restore failed: {restore}")));
        }
        return Err(error);
    }
    Ok(())
}

/// Prune generated trees in every recursive source-discovery pass. Outputs at
/// custom locations are recognized by their runtime manifest as well as by the
/// explicit current destination. Symlinks are classified separately by walkers.
pub(crate) fn generated_dir(path: &Path, excluded: &[PathBuf]) -> bool {
    path.file_name().and_then(|s| s.to_str()).is_some_and(|name|
        name.starts_with('.') || matches!(name, "target" | "build" | "dist"))
        || path.join("run.toml").is_file()
        || path.join("assets/run.toml").is_file()
        || path.join("Contents/MacOS/run.toml").is_file()
        || fs::canonicalize(path).is_ok_and(|p| excluded.iter().any(|e| p.starts_with(e)))
}

fn files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), CookError> {
    for entry in fs::read_dir(dir).map_err(|e| io_at(dir, e))? {
        let entry = entry.map_err(|e| io_at(dir, e))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| io_at(&path, e))?;
        if kind.is_dir() { files(root, &path, out)?; }
        else if kind.is_file() { out.push(path.strip_prefix(root).unwrap().to_owned()); }
        else { return Err(io_at(&path, io::Error::other("payload inventory requires regular files"))); }
    }
    Ok(())
}

/// Hash every relative path and byte after staging. Only the manifest's own
/// `content_hash` is excluded, so configuration and late resources participate.
pub(crate) fn stamp(root: &Path, manifest_path: &Path) -> Result<String, CookError> {
    #[cfg(feature = "cook-metrics")]
    let started = std::time::Instant::now();
    let mut manifest = idtx_runtime_manifest::RuntimeManifest::load(manifest_path)
        .map_err(|e| CookError::RuntimeMetadata(e.to_string()))?;
    manifest.stamp.content_hash = None;
    let normalized = manifest.to_toml_string();
    let mut paths = Vec::new();
    files(root, root, &mut paths)?;
    paths.sort();
    let mut hash = Sha256::new();
    hash.update(b"idtx-payload-inventory-v2\0");
    for relative in &paths {
        let path = root.join(relative);
        let name = relative.to_str().ok_or_else(|| io_at(&path, io::Error::other("payload path is not UTF-8")))?.replace('\\', "/");
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        let (length, digest) = if path == manifest_path {
            (normalized.len() as u64, Sha256::digest(normalized.as_bytes()))
        } else {
            use std::io::Read as _;
            let mut file = fs::File::open(&path).map_err(|e| io_at(&path, e))?;
            let mut content = Sha256::new();
            let mut length = 0_u64;
            let mut buffer = vec![0_u8; 64 * 1024];
            loop {
                let count = file.read(&mut buffer).map_err(|e| io_at(&path, e))?;
                if count == 0 { break; }
                length += count as u64;
                content.update(&buffer[..count]);
            }
            (length, content.finalize())
        };
        hash.update(length.to_le_bytes());
        hash.update(digest);
    }
    let value = hash.finalize().iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    });
    manifest.stamp.content_hash = Some(value.clone());
    fs::write(manifest_path, manifest.to_toml_string()).map_err(|e| io_at(manifest_path, e))?;
    #[cfg(feature = "cook-metrics")]
    tracing::info!(files = paths.len(), elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX), "payload inventory stamped");
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_publication_preserves_bytes_through_rename_and_restore_failures() {
        for fail_restore in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let old = root.path().join("output");
            let new = root.path().join("tree");
            fs::create_dir(&old).unwrap(); fs::create_dir(&new).unwrap();
            fs::write(old.join("value"), b"old").unwrap(); fs::write(new.join("value"), b"new").unwrap();
            let mut calls = 0;
            assert!(replace_with_rollback(&new, &old, |from, to| {
                calls += 1;
                if calls == 2 || (calls == 3 && fail_restore) { return Err(io::Error::other("injected rename failure")); }
                fs::rename(from, to)
            }).is_err());
            let recovery = if fail_restore { root.path().join("previous") } else { old };
            assert_eq!(fs::read(recovery.join("value")).unwrap(), b"old");
            assert_eq!(fs::read(new.join("value")).unwrap(), b"new");
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "subprocess entrypoint for windows_publication_crash_retains_backup"]
    fn windows_publication_crash_child() {
        let root = PathBuf::from(std::env::var_os("IDTX_PUBLISH_CRASH_ROOT").unwrap());
        let mut calls = 0;
        let _ = replace_with_rollback(&root.join("tree"), &root.join("output"), |from, to| {
            calls += 1;
            if calls == 2 { std::process::exit(23); }
            fs::rename(from, to)
        });
        panic!("crash injection was not reached");
    }

    #[cfg(windows)]
    #[test]
    fn windows_publication_crash_retains_backup() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("output")).unwrap();
        fs::create_dir(root.path().join("tree")).unwrap();
        fs::write(root.path().join("output/value"), b"old").unwrap();
        fs::write(root.path().join("tree/value"), b"new").unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "publication::tests::windows_publication_crash_child", "--ignored"])
            .env("IDTX_PUBLISH_CRASH_ROOT", root.path()).status().unwrap();
        assert_eq!(status.code(), Some(23));
        assert!(!root.path().join("output").exists(), "Windows fallback has a documented missing-path window");
        assert_eq!(fs::read(root.path().join("previous/value")).unwrap(), b"old");
        assert_eq!(fs::read(root.path().join("tree/value")).unwrap(), b"new");
    }

    #[test]
    fn abandoned_staging_and_failed_publication_preserve_previous_output() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("good"), b"old").unwrap();
        let pending = Publication::new(&output).unwrap();
        fs::write(pending.path().join("partial"), b"new").unwrap();
        drop(pending);
        assert_eq!(fs::read(output.join("good")).unwrap(), b"old");
        let failed = Publication::new(&output).unwrap();
        fs::remove_dir(failed.path()).unwrap();
        assert!(failed.publish().is_err());
        assert_eq!(fs::read(output.join("good")).unwrap(), b"old");
    }

    #[test]
    fn complete_inventory_includes_late_resources_and_ignores_only_hash_field() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("run.toml");
        fs::write(&manifest, idtx_runtime_manifest::RuntimeManifest::default().to_toml_string()).unwrap();
        let first = stamp(root.path(), &manifest).unwrap();
        assert_eq!(first, stamp(root.path(), &manifest).unwrap());
        fs::create_dir(root.path().join("usd-schemas")).unwrap();
        let plugin = root.path().join("usd-schemas/plugInfo.json");
        fs::write(&plugin, b"schema-v1").unwrap();
        let second = stamp(root.path(), &manifest).unwrap();
        assert_ne!(first, second);
        fs::write(&plugin, b"schema-v2").unwrap();
        assert_ne!(second, stamp(root.path(), &manifest).unwrap());
    }
}
