//! Project-root sandbox guard.
//!
//! [`ProjectRoot`] is the canonicalised absolute path that bounds a project.
//! Path resolution rejects lexical escapes and validates the nearest existing
//! ancestor of a new target. A returned path is a filesystem snapshot, not a
//! write capability: use [`ProjectRoot::prepare_file`] for file writes, which
//! remain relative to an open root directory even if an ancestor is replaced.

use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use cap_std::fs::{Dir, OpenOptions};

/// A canonicalised project root directory that acts as a sandbox boundary.
#[derive(Debug, Clone)]
pub struct ProjectRoot {
    path: PathBuf,
    directory: Arc<Dir>,
}

impl ProjectRoot {
    /// Open an already-canonical absolute root directory. Failure to establish
    /// the boundary is an error, never a fallback to an unchecked path.
    pub fn from_canonical(root: PathBuf) -> io::Result<Self> {
        if !root.is_absolute() || std::fs::canonicalize(&root)? != root {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "project root must be canonical and absolute"));
        }
        let directory = Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
        Ok(Self { path: root, directory: Arc::new(directory) })
    }

    /// Resolve a project root from an optional CLI hint:
    /// - a directory hint is used directly,
    /// - a file hint uses its parent directory,
    /// - `None` falls back to the current working directory.
    ///
    /// The result is canonicalised.
    ///
    /// # Errors
    /// Propagates I/O errors from resolving the current directory or
    /// canonicalising the chosen root.
    pub fn resolve_from_cli(cli_hint: Option<&Path>) -> std::io::Result<Self> {
        let base = match cli_hint {
            Some(p) if std::fs::metadata(p)?.is_dir() => p.to_path_buf(),
            Some(p) => p
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf),
            None => std::env::current_dir()?,
        };
        Self::from_canonical(std::fs::canonicalize(base)?)
    }

    /// The root path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// Join a project-relative path onto the root, rejecting escapes.
    ///
    /// Missing suffixes are accepted only below an existing in-root directory.
    /// Dangling links and errors other than absence fail closed. The returned
    /// path is suitable for display/read discovery; use [`Self::prepare_file`]
    /// to enforce the boundary at the actual file-open operation.
    #[must_use]
    pub fn resolve_relative(&self, rel: &Path) -> Option<PathBuf> {
        self.resolve_relative_checked(rel).ok()
    }

    /// Error-preserving form of [`Self::resolve_relative`]. Only `NotFound`
    /// permits walking up to an existing ancestor; a dangling link is an
    /// existing directory entry and is therefore not treated as a missing suffix.
    pub fn resolve_relative_checked(&self, rel: &Path) -> io::Result<PathBuf> {
        validate_relative(rel)?;
        self.resolve_contained(&self.path.join(rel))
    }

    fn resolve_contained(&self, path: &Path) -> io::Result<PathBuf> {
        let mut ancestor = path.to_path_buf();
        let mut missing = Vec::new();
        loop {
            match std::fs::canonicalize(&ancestor) {
                Ok(mut canonical) => {
                    if !canonical.starts_with(&self.path) { return Err(escape_error()); }
                    if !missing.is_empty() && !std::fs::metadata(&canonical)?.is_dir() {
                        return Err(io::ErrorKind::NotADirectory.into());
                    }
                    for component in missing.iter().rev() { canonical.push(component); }
                    return Ok(canonical);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match std::fs::symlink_metadata(&ancestor) {
                        // In particular, a dangling symlink is NOT a new path.
                        Ok(_) => return Err(error),
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e),
                    }
                    let component = ancestor.file_name().ok_or_else(escape_error)?.to_os_string();
                    missing.push(component);
                    if !ancestor.pop() { return Err(error); }
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Convert a project-relative or absolute in-project selection to its
    /// canonical project-relative form, including not-yet-created files.
    pub fn relative_path(&self, path: &Path) -> io::Result<PathBuf> {
        let resolved = if path.is_absolute() {
            if path.components().any(|c| matches!(c, Component::ParentDir)) { return Err(escape_error()); }
            // Handles platform path aliases (e.g. /var vs /private/var and
            // Windows verbatim prefixes) before checking the canonical boundary.
            self.resolve_contained(path)?
        } else {
            self.resolve_relative_checked(path)?
        };
        Ok(resolved.strip_prefix(&self.path).map_err(|_| escape_error())?.to_path_buf())
    }

    /// Validate a file target and retain its root directory capability. Actual
    /// opens remain confined even if a symlink is installed after this call.
    pub fn prepare_file(&self, rel: &Path) -> io::Result<ProjectFileTarget> {
        let path = self.resolve_relative_checked(rel)?;
        let relative = path.strip_prefix(&self.path).map_err(|_| escape_error())?.to_path_buf();
        if relative.file_name().is_none() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "expected a project file name"));
        }
        Ok(ProjectFileTarget { directory: self.directory.clone(), relative, path })
    }

    /// Create project-relative directories using the open root, not an ambient
    /// absolute path whose ancestors could have changed after validation.
    pub fn create_dir_all(&self, rel: &Path) -> io::Result<PathBuf> {
        let path = self.resolve_relative_checked(rel)?;
        let relative = path.strip_prefix(&self.path).map_err(|_| escape_error())?;
        self.directory.create_dir_all(relative)?;
        Ok(path)
    }

    /// Exclusively create one new child directory and retain its handle. Used
    /// for a new project so an existing entry (including a dangling link) cannot
    /// be reused between an `exists` check and scaffold writes.
    pub(crate) fn create_child(&self, name: &Path) -> io::Result<Self> {
        validate_relative(name)?;
        if name.components().count() != 1 || !matches!(name.components().next(), Some(Component::Normal(_))) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "expected one directory name"));
        }
        self.directory.create_dir(name)?;
        let directory = self.directory.open_dir(name)?;
        Ok(Self { path: self.path.join(name), directory: Arc::new(directory) })
    }

    /// A short display label — the last two path components only, so an absolute
    /// project path is never leaked wholesale into the UI.
    #[must_use]
    pub fn display_label(&self) -> String {
        let tail: Vec<_> = self
            .path
            .components()
            .rev()
            .take(2)
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        if tail.is_empty() {
            normalize_display_path(&self.path)
        } else {
            tail.into_iter().rev().collect::<Vec<_>>().join("/")
        }
    }
}

fn escape_error() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "path escapes the project root")
}

fn validate_relative(path: &Path) -> io::Result<()> {
    if path.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) {
        return Err(escape_error());
    }
    Ok(())
}

/// A validated file name plus a retained root directory handle. This is the
/// write boundary; [`Self::path`] alone is not a capability. Absolute in-root
/// symlinks encountered during validation are canonicalized; later symlink
/// changes are handled by `cap-std`'s confined directory-relative operations.
#[derive(Debug, Clone)]
pub struct ProjectFileTarget {
    directory: Arc<Dir>,
    relative: PathBuf,
    path: PathBuf,
}

impl ProjectFileTarget {
    /// Location snapshot for display/path-only consumers, not a write capability.
    #[must_use]
    pub fn path(&self) -> &Path { &self.path }

    fn open_write(&self, create_new: bool) -> io::Result<cap_std::fs::File> {
        if let Some(parent) = self.relative.parent() { self.directory.create_dir_all(parent)?; }
        self.directory.open_with(&self.relative,
            OpenOptions::new().write(true).create(true).truncate(true).create_new(create_new))
    }

    /// Write a complete file through the confined handle, replacing its content.
    pub fn write(&self, bytes: impl AsRef<[u8]>) -> io::Result<()> {
        self.open_write(false)?.write_all(bytes.as_ref())
    }

    /// Create a new file without clobbering any existing entry, including links.
    pub fn write_new(&self, bytes: impl AsRef<[u8]>) -> io::Result<()> {
        self.open_write(true)?.write_all(bytes.as_ref())
    }

    /// Read a file using the same retained directory boundary.
    pub fn read_to_string(&self) -> io::Result<String> {
        self.directory.read_to_string(&self.relative)
    }

    /// Copy a user-selected external file into a new, confined destination.
    pub fn copy_new_from(&self, source: &Path) -> io::Result<u64> {
        self.copy_from(source, true)
    }

    /// Replace a confined file with the output of a path-only producer (e.g.
    /// `OpenUSD`) that generated its artifact in private scratch storage.
    pub fn copy_from(&self, source: &Path, create_new: bool) -> io::Result<u64> {
        let mut input = std::fs::File::open(source)?;
        let mut output = self.open_write(create_new)?;
        match io::copy(&mut input, &mut output) {
            Ok(bytes) => Ok(bytes),
            Err(error) => {
                drop(output);
                if create_new { let _ = self.remove(); }
                Err(error)
            }
        }
    }

    /// Remove the directory entry without following its final symlink. Used to
    /// clean up an authored artifact if its associated stage edit fails.
    pub fn remove(&self) -> io::Result<()> { self.directory.remove_file(&self.relative) }
}

/// Strip Windows verbatim (`\\?\`) prefixes from a path for display.
#[must_use]
pub fn normalize_display_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(stripped) => stripped.to_owned(),
        None => s.into_owned(),
    }
}

/// Strip a Windows verbatim (`\\?\`, `\\?\UNC\`) prefix, yielding a plain path
/// that still round-trips through the OS but relativizes against non-verbatim
/// paths and anchors relative references correctly once handed to `OpenUSD`.
///
/// `std::fs::canonicalize` returns verbatim (`\\?\C:\…`) paths on Windows; those
/// share no comparable drive-`Prefix` component with an ordinary `C:\…` path, so
/// relativization falls back to a raw `//?/C:/…` string — which USD accepts as a
/// direct layer identifier but **cannot** anchor a relative reference against.
/// Normalizing at the asset-path boundary keeps that prefix out of authored USD
/// arcs. No-op on non-verbatim paths and on non-Windows.
#[must_use]
pub fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    // `\\?\UNC\server\share` denotes `\\server\share`; restore the double-slash.
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

/// Format a stage path for display: project-relative when it lives inside the
/// project root, otherwise the normalised absolute path.
#[must_use]
pub fn format_stage_display(stage_path: &Path, project_root: &Path, sandboxed: bool) -> String {
    if sandboxed
        && let Ok(rel) = stage_path.strip_prefix(project_root) {
            return rel.to_string_lossy().into_owned();
        }
    normalize_display_path(stage_path)
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
            "idtx-projectroot-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    #[test]
    fn resolves_relative_inside_root() {
        let root = ProjectRoot::from_canonical(scratch("inside")).unwrap();
        let resolved = root
            .resolve_relative(Path::new("stages/main.usda"))
            .expect("inside");
        assert!(resolved.starts_with(root.as_path()));
    }

    #[test]
    fn rejects_absolute_and_parent_escapes() {
        let root = ProjectRoot::from_canonical(scratch("escape")).unwrap();
        assert!(root.resolve_relative(Path::new("/etc/passwd")).is_none());
        assert!(
            root.resolve_relative(Path::new("../../etc/passwd"))
                .is_none()
        );
        assert!(root.resolve_relative(Path::new("a/../../b")).is_none());
    }

    #[test]
    fn curdir_components_are_allowed() {
        let root = ProjectRoot::from_canonical(scratch("curdir")).unwrap();
        assert!(root.resolve_relative(Path::new("./a/./b.usda")).is_some());
    }

    #[test]
    fn display_label_uses_tail() {
        let dir = scratch("display").join("projects/demo");
        std::fs::create_dir_all(&dir).unwrap();
        let root = ProjectRoot::from_canonical(dir).unwrap();
        assert_eq!(root.display_label(), "projects/demo");
    }

    #[test]
    fn strip_verbatim_prefix_normalizes_windows_verbatim_paths() {
        // Verbatim disk → plain disk path.
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\C:\a\b")),
            PathBuf::from(r"C:\a\b")
        );
        // Verbatim UNC → plain UNC path (double leading slash restored).
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\UNC\server\share\x")),
            PathBuf::from(r"\\server\share\x")
        );
        // Ordinary Windows and POSIX paths pass through untouched.
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"C:\a\b")),
            PathBuf::from(r"C:\a\b")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new("/proj/levels/hub.usda")),
            PathBuf::from("/proj/levels/hub.usda")
        );
    }
}
