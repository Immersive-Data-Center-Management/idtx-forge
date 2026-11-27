//! Project-root asset inventory.
//!
//! A cheap, sandbox-confined directory listing of the files under a project
//! root — the model behind the Conductor's Asset Browser and the feed for
//! project-relative reference/payload authoring. Classification is
//! extension-only (no file contents are read), so a scan is a single
//! `read_dir` per visited directory.
//!
//! This lives in `idtx-project` (not the render-oriented `idtx-assets` cache)
//! because it is fundamentally a *project* operation: every path is resolved
//! through [`ProjectRoot`](crate::ProjectRoot) so the browser can never escape
//! the sandbox, and the search roots come from the project manifest.

use std::path::{Path, PathBuf};

/// Broad classification of a project asset, derived from its file extension
/// (directories are always [`AssetKind::Folder`]).
///
/// Drives the asset-browser glyph and, for [`AssetKind::Stage`], whether an
/// entry can be dragged onto a prim as a reference/payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetKind {
    /// A directory.
    Folder,
    /// A USD stage (`.usd`/`.usda`/`.usdc`/`.usdz`) — referenceable/payloadable.
    Stage,
    /// A raster/HDR texture (`.png`/`.jpg`/`.exr`/…).
    Texture,
    /// A `MaterialX` document (`.mtlx`).
    Material,
    /// A mesh/model interchange file (`.gltf`/`.glb`/`.obj`/`.fbx`/…).
    Model,
    /// An audio clip (`.wav`/`.mp3`/…).
    Audio,
    /// A video clip (`.mp4`/`.mov`/…).
    Video,
    /// A structured-data / config sidecar (`.toml`/`.json`/…).
    Data,
    /// Anything else.
    Other,
}

impl AssetKind {
    /// Classify a file by its path's extension. Returns [`AssetKind::Other`]
    /// for a path with no extension. Never returns [`AssetKind::Folder`] —
    /// directories are classified by the scanner, which knows the file type.
    #[must_use]
    pub fn for_path(path: &Path) -> Self {
        path.extension()
            .and_then(|e| e.to_str())
            .map_or(Self::Other, Self::for_extension)
    }

    /// Classify by a bare extension (without the dot), case-insensitively.
    #[must_use]
    pub fn for_extension(ext: &str) -> Self {
        match ext.to_ascii_lowercase().as_str() {
            "usd" | "usda" | "usdc" | "usdz" => Self::Stage,
            "png" | "jpg" | "jpeg" | "exr" | "hdr" | "tif" | "tiff" | "tga" | "bmp" | "tx"
            | "ktx" | "ktx2" | "dds" | "webp" => Self::Texture,
            "mtlx" => Self::Material,
            "gltf" | "glb" | "obj" | "fbx" | "abc" | "ply" | "stl" => Self::Model,
            "wav" | "mp3" | "ogg" | "flac" | "aac" | "m4a" => Self::Audio,
            "mp4" | "mov" | "avi" | "webm" | "mkv" => Self::Video,
            "toml" | "json" | "txt" | "csv" | "yaml" | "yml" | "xml" => Self::Data,
            _ => Self::Other,
        }
    }

    /// Whether this asset is a USD stage that can be referenced/payloaded.
    #[must_use]
    pub fn is_stage(self) -> bool {
        matches!(self, Self::Stage)
    }

    /// A short human label for tooltips / menus.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Folder => "Folder",
            Self::Stage => "USD Stage",
            Self::Texture => "Texture",
            Self::Material => "Material",
            Self::Model => "Model",
            Self::Audio => "Audio",
            Self::Video => "Video",
            Self::Data => "Data",
            Self::Other => "File",
        }
    }
}

/// One entry in a project directory listing — a UI-neutral snapshot the shell
/// turns into an asset-browser row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetEntry {
    /// Path relative to the project root (native separators).
    pub rel_path: PathBuf,
    /// Final path component (file or directory name), for display.
    pub name: String,
    /// Classification ([`AssetKind::Folder`] for directories).
    pub kind: AssetKind,
    /// `true` for directories.
    pub is_dir: bool,
}

/// List the immediate children of `subdir` (project-relative; empty = the
/// project root) inside a sandbox rooted at `root`.
///
/// Directories sort before files, each group alphabetically
/// (case-insensitive). Hidden entries (leading `.`) and the `project.toml`
/// manifest are skipped. Returns an empty vec when the directory is missing,
/// unreadable, or `subdir` escapes the sandbox — the caller renders that as an
/// empty folder rather than an error.
#[must_use]
pub fn list_dir(root: &crate::ProjectRoot, subdir: &Path) -> Vec<AssetEntry> {
    let Some(abs_dir) = root.resolve_relative(subdir) else {
        return Vec::new();
    };
    let Ok(read) = std::fs::read_dir(&abs_dir) else {
        return Vec::new();
    };
    let mut entries: Vec<AssetEntry> = Vec::new();
    for dirent in read.flatten() {
        let name = dirent.file_name().to_string_lossy().into_owned();
        // Skip hidden entries and the manifest sidecar.
        if name.starts_with('.') || name == crate::PROJECT_MANIFEST_FILENAME {
            continue;
        }
        let is_dir = dirent.file_type().is_ok_and(|t| t.is_dir());
        let kind = if is_dir {
            AssetKind::Folder
        } else {
            AssetKind::for_path(Path::new(&name))
        };
        entries.push(AssetEntry {
            rel_path: subdir.join(&name),
            name,
            kind,
            is_dir,
        });
    }
    entries.sort_by(|a, b| {
        // Directories first, then case-insensitive name order.
        b.is_dir.cmp(&a.is_dir).then_with(|| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        })
    });
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProjectRoot;

    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "idtx-assets-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn asset_kind_classifies_by_extension() {
        assert_eq!(AssetKind::for_extension("usda"), AssetKind::Stage);
        assert_eq!(AssetKind::for_extension("USDZ"), AssetKind::Stage);
        assert_eq!(AssetKind::for_extension("png"), AssetKind::Texture);
        assert_eq!(AssetKind::for_extension("EXR"), AssetKind::Texture);
        assert_eq!(AssetKind::for_extension("mtlx"), AssetKind::Material);
        assert_eq!(AssetKind::for_extension("glb"), AssetKind::Model);
        assert_eq!(AssetKind::for_extension("wav"), AssetKind::Audio);
        assert_eq!(AssetKind::for_extension("mp4"), AssetKind::Video);
        assert_eq!(AssetKind::for_extension("toml"), AssetKind::Data);
        assert_eq!(AssetKind::for_extension("xyz"), AssetKind::Other);
    }

    #[test]
    fn asset_kind_for_path_handles_missing_extension() {
        assert_eq!(AssetKind::for_path(Path::new("README")), AssetKind::Other);
        assert_eq!(
            AssetKind::for_path(Path::new("a/b/prop.usdc")),
            AssetKind::Stage
        );
        assert!(AssetKind::for_path(Path::new("x.usd")).is_stage());
    }

    #[test]
    fn list_dir_sorts_directories_before_files_alphabetically() {
        let dir = scratch("sort");
        std::fs::create_dir_all(dir.join("zeta")).unwrap();
        std::fs::create_dir_all(dir.join("alpha")).unwrap();
        std::fs::write(dir.join("b.usda"), "#usda 1.0\n").unwrap();
        std::fs::write(dir.join("A.png"), []).unwrap();
        let root = ProjectRoot::from_canonical(std::fs::canonicalize(&dir).unwrap()).unwrap();
        let entries = list_dir(&root, Path::new(""));
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "zeta", "A.png", "b.usda"]);
        assert!(entries[0].is_dir && entries[0].kind == AssetKind::Folder);
        assert_eq!(entries[2].kind, AssetKind::Texture);
        assert_eq!(entries[3].kind, AssetKind::Stage);
    }

    #[test]
    fn list_dir_skips_hidden_and_manifest() {
        let dir = scratch("hidden");
        std::fs::write(dir.join(crate::PROJECT_MANIFEST_FILENAME), "").unwrap();
        std::fs::write(dir.join(".hidden"), "").unwrap();
        std::fs::write(dir.join("keep.usda"), "").unwrap();
        let root = ProjectRoot::from_canonical(std::fs::canonicalize(&dir).unwrap()).unwrap();
        let entries = list_dir(&root, Path::new(""));
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["keep.usda"]);
    }

    #[test]
    fn list_dir_reads_nested_subdir_with_relative_paths() {
        let dir = scratch("nested");
        std::fs::create_dir_all(dir.join("assets/props")).unwrap();
        std::fs::write(dir.join("assets/props/chair.usda"), "").unwrap();
        let root = ProjectRoot::from_canonical(std::fs::canonicalize(&dir).unwrap()).unwrap();
        let entries = list_dir(&root, Path::new("assets/props"));
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].rel_path,
            PathBuf::from("assets/props/chair.usda")
        );
        assert_eq!(entries[0].kind, AssetKind::Stage);
    }

    #[test]
    fn list_dir_missing_or_escaping_is_empty() {
        let dir = scratch("missing");
        let root = ProjectRoot::from_canonical(std::fs::canonicalize(&dir).unwrap()).unwrap();
        assert!(list_dir(&root, Path::new("does/not/exist")).is_empty());
        // A `..` escape is rejected by the sandbox → empty, never a peek outside.
        assert!(list_dir(&root, Path::new("../..")).is_empty());
    }
}
