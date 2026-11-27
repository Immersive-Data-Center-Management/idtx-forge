//! Persistence for the **File → Open Recent…** list *and* the Conductor's
//! recent-projects list shown in the startup dialog.
//!
//! Both lists live in a single `~/.idtx-studio/recent.toml` (the same directory
//! as the server config in `idtx-remote`): the `recent` key holds up to
//! [`MAX_RECENT`] most-recently-opened **local** stage paths, and the
//! `recent_projects` key holds up to [`MAX_RECENT`] most-recently-activated
//! project-manifest (`project.toml`) paths. Remote stages are never recorded —
//! the caller only invokes [`push_recent`] on a local open.
//!
//! Because both lists share one file, the `save_*` helpers read the current
//! on-disk file first and rewrite only their own key, so persisting one list
//! never clobbers the other.
//!
//! All I/O here is best-effort: a failure to read or write either list must
//! never break stage opening, project activation, or app startup, so errors are
//! logged and swallowed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Maximum number of entries kept in each recent list.
pub(crate) const MAX_RECENT: usize = 8;

/// On-disk TOML shape: `recent = ["/path/a.usda", …]` plus
/// `recent_projects = ["/path/project.toml", …]`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct RecentFile {
    #[serde(default)]
    recent: Vec<String>,
    #[serde(default)]
    recent_projects: Vec<String>,
}

/// `~/.idtx-studio/recent.toml`, or `None` if the home directory is unknown.
fn recent_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".idtx-studio").join("recent.toml"))
}

/// Reads and parses the whole recent file. Returns a default (both lists empty)
/// on any error or if the file does not exist.
fn read_recent_file() -> RecentFile {
    let Some(path) = recent_path() else {
        return RecentFile::default();
    };
    if !path.exists() {
        return RecentFile::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str::<RecentFile>(&text) {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!(path = %path.display(), "failed to parse recent.toml: {e}");
                RecentFile::default()
            }
        },
        Err(e) => {
            tracing::warn!(path = %path.display(), "failed to read recent.toml: {e}");
            RecentFile::default()
        }
    }
}

/// Serializes `file` to `~/.idtx-studio/recent.toml`. Errors are logged and
/// swallowed — writing a recent list must never abort a stage open or a project
/// activation.
fn write_recent_file(file: &RecentFile) {
    let Some(path) = recent_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(dir = %dir.display(), "failed to create config dir: {e}");
            return;
        }
    }
    match toml::to_string_pretty(file) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&path, text) {
                tracing::warn!(path = %path.display(), "failed to write recent.toml: {e}");
            }
        }
        Err(e) => tracing::warn!("failed to serialize recent list: {e}"),
    }
}

/// Reads the persisted recent-stages list. Returns an empty list on any error
/// or if the file does not exist.
pub(crate) fn load_recent() -> Vec<PathBuf> {
    read_recent_file()
        .recent
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

/// Reads the persisted recent-projects list (project-manifest paths). Returns an
/// empty list on any error or if the file does not exist.
pub(crate) fn load_recent_projects() -> Vec<PathBuf> {
    read_recent_file()
        .recent_projects
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

/// Persists the recent-stages `list`, preserving the stored recent-projects
/// list. Errors are logged and swallowed.
pub(crate) fn save_recent(list: &[PathBuf]) {
    let mut file = read_recent_file();
    file.recent = list.iter().map(|p| p.display().to_string()).collect();
    write_recent_file(&file);
}

/// Persists the recent-projects `list`, preserving the stored recent-stages
/// list. Errors are logged and swallowed.
pub(crate) fn save_recent_projects(list: &[PathBuf]) {
    let mut file = read_recent_file();
    file.recent_projects = list.iter().map(|p| p.display().to_string()).collect();
    write_recent_file(&file);
}

/// Inserts `path` at the front of `list`, removing any prior occurrence
/// (de-duplication) and capping the list at [`MAX_RECENT`] entries.
pub(crate) fn push_recent(list: &mut Vec<PathBuf>, path: &Path) {
    list.retain(|p| p != path);
    list.insert(0, path.to_path_buf());
    list.truncate(MAX_RECENT);
}

/// Inserts a project-manifest `path` at the front of the recent-projects
/// `list`, de-duplicating and capping at [`MAX_RECENT`]. Identical policy to
/// [`push_recent`]; separate name documents the distinct list it mutates.
pub(crate) fn push_recent_project(list: &mut Vec<PathBuf>, path: &Path) {
    push_recent(list, path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_dedupes_and_moves_to_front() {
        let mut list = vec![PathBuf::from("/a.usda"), PathBuf::from("/b.usda")];
        push_recent(&mut list, Path::new("/b.usda"));
        assert_eq!(
            list,
            vec![PathBuf::from("/b.usda"), PathBuf::from("/a.usda")]
        );
    }

    #[test]
    fn push_prepends_new_entry() {
        let mut list = vec![PathBuf::from("/a.usda")];
        push_recent(&mut list, Path::new("/c.usda"));
        assert_eq!(list.first(), Some(&PathBuf::from("/c.usda")));
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn push_caps_at_max_recent() {
        let mut list = Vec::new();
        for i in 0..(MAX_RECENT + 5) {
            push_recent(&mut list, Path::new(&format!("/f{i}.usda")));
        }
        assert_eq!(list.len(), MAX_RECENT);
        // Most-recently pushed entry is at the front, oldest evicted.
        assert_eq!(
            list.first(),
            Some(&PathBuf::from(format!("/f{}.usda", MAX_RECENT + 4)))
        );
        assert!(!list.contains(&PathBuf::from("/f0.usda")));
    }

    #[test]
    fn toml_round_trip_preserves_order() {
        let original = RecentFile {
            recent: vec!["/x.usda".to_owned(), "/y.usdc".to_owned()],
            recent_projects: vec!["/proj/project.toml".to_owned()],
        };
        let text = toml::to_string_pretty(&original).unwrap();
        let parsed: RecentFile = toml::from_str(&text).unwrap();
        assert_eq!(parsed.recent, original.recent);
        assert_eq!(parsed.recent_projects, original.recent_projects);
    }

    #[test]
    fn parse_missing_recent_key_yields_empty() {
        let parsed: RecentFile = toml::from_str("").unwrap();
        assert!(parsed.recent.is_empty());
        assert!(parsed.recent_projects.is_empty());
    }

    #[test]
    fn parse_legacy_file_without_projects_key() {
        // A `recent.toml` written before the projects list existed must still
        // parse (projects default to empty) rather than wiping the stages list.
        let parsed: RecentFile = toml::from_str("recent = [\"/a.usda\"]").unwrap();
        assert_eq!(parsed.recent, vec!["/a.usda".to_owned()]);
        assert!(parsed.recent_projects.is_empty());
    }

    #[test]
    fn push_recent_project_dedupes_and_moves_to_front() {
        let mut list = vec![
            PathBuf::from("/a/project.toml"),
            PathBuf::from("/b/project.toml"),
        ];
        push_recent_project(&mut list, Path::new("/b/project.toml"));
        assert_eq!(
            list,
            vec![
                PathBuf::from("/b/project.toml"),
                PathBuf::from("/a/project.toml")
            ]
        );
    }
}
