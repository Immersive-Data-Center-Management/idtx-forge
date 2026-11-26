//! Author-owned source collections. Snapshots preserve module and include paths;
//! generated Cargo files never occupy the author's namespace.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{BuildError, Result, fingerprint::Fingerprint, simplified, skeleton};

/// One behavior entry and its source collection. Ordinary Rust modules and
/// source-relative include data must stay inside this collection.
#[derive(Debug, Clone)]
pub struct BehaviorSource {
    entry: PathBuf,
    root: PathBuf,
}

impl BehaviorSource {
    /// Use an explicit root, or the nearest enclosing `behaviors/` directory.
    /// A source outside a `behaviors/` tree uses its own parent directory.
    pub fn discover(entry: &Path, root: Option<&Path>) -> Result<Self> {
        if !entry.is_file() { return Err(BuildError::SourceNotFound(entry.to_path_buf())); }
        if !std::fs::symlink_metadata(entry)?.is_file() {
            return Err(BuildError::InvalidInput(format!("behavior entry must be a regular file: {}", entry.display())));
        }
        let entry = simplified(std::fs::canonicalize(entry)?);
        let parent = entry.parent().ok_or_else(|| BuildError::InvalidInput("source has no parent".into()))?;
        let root = match root {
            Some(root) => simplified(std::fs::canonicalize(root)?),
            None => parent.ancestors().find(|p| p.file_name().is_some_and(|n| n == "behaviors"))
                .unwrap_or(parent).to_path_buf(),
        };
        if !root.is_dir() || !entry.starts_with(&root) {
            return Err(BuildError::InvalidInput("behavior entry must be inside its source collection".into()));
        }
        Ok(Self { entry, root })
    }

    #[must_use]
    pub fn entry(&self) -> &Path { &self.entry }

    #[must_use]
    pub fn root(&self) -> &Path { &self.root }

    #[must_use]
    pub fn relative_entry(&self) -> &Path {
        self.entry.strip_prefix(&self.root).expect("validated source collection")
    }

    /// Portable publication identity, relative to the enclosing project when
    /// present, otherwise to the source collection. Machine-local workspace
    /// identity is maintained separately by the build recipe.
    #[must_use]
    pub fn name(&self) -> String {
        let relative = self.root.ancestors().find(|root| root.join("project.toml").is_file())
            .and_then(|root| self.entry.strip_prefix(root).ok()).map(Path::to_path_buf)
            .unwrap_or_else(|| Path::new(self.root.file_name().unwrap_or_default()).join(self.relative_entry()));
        self.name_for(&relative)
    }

    /// Include level/collection identity when publishing several collections
    /// into one payload. The base itself never contributes to the name.
    pub fn name_relative_to(&self, base: &Path) -> Result<String> {
        let base = simplified(std::fs::canonicalize(base)?);
        let relative = self.entry.strip_prefix(base).map_err(|_| BuildError::InvalidInput(
            "behavior publication identity must be inside its project/asset root".into()))?;
        Ok(self.name_for(relative))
    }

    fn name_for(&self, relative: &Path) -> String {
        let stem = self.entry.file_stem().and_then(|s| s.to_str()).unwrap_or("behavior");
        let prefix = if stem == "behavior" {
            self.entry.parent().and_then(Path::file_name).and_then(|s| s.to_str()).unwrap_or(stem)
        } else { stem };
        let prefix = crate::crate_name(prefix);
        let mut hash = Fingerprint::new();
        hash.add("source-path-v2", relative.to_string_lossy().replace('\\', "/").as_bytes());
        format!("{}_{}", prefix.chars().take(40).collect::<String>(), crate::path_segment(&hash.finish()))
    }

    /// Clone the complete authored collection into a new directory. Used for
    /// imports and local copies, so nested/shared modules travel with the entry.
    /// Existing destinations are never overwritten.
    pub fn copy_to(&self, destination: &Path) -> Result<PathBuf> {
        let snapshot = self.snapshot(&[])?;
        std::fs::create_dir(destination)?;
        if let Err(error) = snapshot.stage(destination) {
            let _ = std::fs::remove_dir_all(destination);
            return Err(error);
        }
        Ok(destination.join(self.relative_entry()))
    }

    pub(crate) fn snapshot(&self, excluded: &[PathBuf]) -> Result<SourceSnapshot> {
        let mut files = BTreeMap::new();
        collect(&self.root, &self.root, excluded, &mut files)?;
        if !files.contains_key(self.relative_entry()) {
            return Err(BuildError::InvalidInput(format!("entry is in an excluded/generated source directory: {}", self.entry.display())));
        }
        Ok(SourceSnapshot { files })
    }
}

// A collection conservatively includes regular source and data files. Build
// output, editor/VCS metadata and compiled modules are not authoring inputs.
fn generated(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n|
        n.starts_with('.') || matches!(n, "target" | "build" | "dist"))
        || path.extension().is_some_and(|e| e == "wasm")
}

fn collect(root: &Path, directory: &Path, excluded: &[PathBuf], files: &mut BTreeMap<PathBuf, Vec<u8>>) -> Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if generated(&path) || excluded.iter().any(|p| path.starts_with(p)) { continue; }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(root, &path, excluded, files)?;
        } else if kind.is_file() {
            files.insert(path.strip_prefix(root).expect("source descendant").to_path_buf(), std::fs::read(&path)?);
        } else {
            return Err(BuildError::InvalidInput(format!("source collection requires regular files and directories (no symlinks): {}", path.display())));
        }
    }
    Ok(())
}

pub(crate) struct SourceSnapshot {
    files: BTreeMap<PathBuf, Vec<u8>>,
}

impl SourceSnapshot {
    pub(crate) fn hash(&self) -> String {
        let mut hash = Fingerprint::new();
        for (path, bytes) in &self.files {
            hash.add("path", path.as_os_str().as_encoded_bytes());
            hash.add("content", bytes);
        }
        hash.finish()
    }

    pub(crate) fn stage(&self, destination: &Path) -> Result<()> {
        std::fs::create_dir_all(destination)?;
        if !std::fs::symlink_metadata(destination)?.is_dir() {
            return Err(BuildError::InvalidInput(format!("snapshot must be a regular directory: {}", destination.display())));
        }
        self.prune(destination, destination)?;
        for (relative, bytes) in &self.files {
            let path = destination.join(relative);
            std::fs::create_dir_all(path.parent().expect("staged source parent"))?;
            skeleton::write_if_changed(&path, bytes)?;
        }
        Ok(())
    }

    fn prune(&self, root: &Path, directory: &Path) -> Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path.strip_prefix(root).expect("snapshot descendant");
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if self.files.keys().any(|p| p != relative && p.starts_with(relative)) {
                    self.prune(root, &path)?;
                } else { std::fs::remove_dir_all(path)?; }
            } else if kind.is_file() {
                if !self.files.contains_key(relative) { std::fs::remove_file(path)?; }
            } else {
                return Err(BuildError::InvalidInput(format!("generated snapshot is not a regular file/directory: {}", path.display())));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_sources_data_and_reserved_names_survive_copy_and_stale_files_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("behaviors");
        std::fs::create_dir_all(root.join("spin/motion")).unwrap();
        std::fs::create_dir_all(root.join("shared")).unwrap();
        for path in ["spin/behavior.rs", "spin/motion.rs", "spin/motion/easing.rs", "shared/lib.rs", "shared/data.bin"] {
            std::fs::write(root.join(path), path.as_bytes()).unwrap();
        }
        let source = BehaviorSource::discover(&root.join("spin/behavior.rs"), None).unwrap();
        assert_eq!(source.root(), simplified(std::fs::canonicalize(&root).unwrap()));
        let destination = dir.path().join("copy");
        let copied = source.copy_to(&destination).unwrap();
        assert_eq!(copied, destination.join("spin/behavior.rs"));
        assert_eq!(source.snapshot(&[]).unwrap().hash(), BehaviorSource::discover(&copied, Some(&destination)).unwrap().snapshot(&[]).unwrap().hash());
        assert!(source.copy_to(&destination).is_err(), "imports cannot overwrite author files");
        std::fs::remove_file(root.join("spin/motion/easing.rs")).unwrap();
        source.snapshot(&[]).unwrap().stage(&destination).unwrap();
        assert!(!destination.join("spin/motion").exists());
        assert!(destination.join("shared/lib.rs").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn source_symlinks_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("behavior.rs"), "").unwrap();
        std::os::unix::fs::symlink("behavior.rs", dir.path().join("helper.rs")).unwrap();
        let source = BehaviorSource::discover(&dir.path().join("behavior.rs"), None).unwrap();
        assert!(source.snapshot(&[]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn staging_refuses_a_symlink_to_author_files() {
        let dir = tempfile::tempdir().unwrap();
        let author = dir.path().join("author");
        std::fs::create_dir(&author).unwrap();
        std::fs::write(author.join("behavior.rs"), "// original").unwrap();
        let snapshot = BehaviorSource::discover(&author.join("behavior.rs"), None).unwrap().snapshot(&[]).unwrap();
        let destination = dir.path().join("snapshot");
        std::os::unix::fs::symlink(&author, &destination).unwrap();
        assert!(snapshot.stage(&destination).is_err());
        assert_eq!(std::fs::read_to_string(author.join("behavior.rs")).unwrap(), "// original");
    }
}
