//! Shared bundle/session bootstrap for every runtime adapter.
use std::path::{Path, PathBuf};
use idtx_behavior::{InputMap, Limits};
use idtx_studio_session::StudioSession;

#[cfg(feature = "metrics")]
pub(crate) fn peak_rss_bytes() -> u64 {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: getrusage initializes the correctly sized output on success.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0 {
            // SAFETY: successful getrusage initialized the output.
            let rss = u64::try_from(unsafe { usage.assume_init() }.ru_maxrss).unwrap_or(0);
            return if cfg!(target_vendor = "apple") { rss } else { rss.saturating_mul(1024) };
        }
    }
    0
}

#[derive(Debug, Clone)]
pub struct BundleContext { pub root: PathBuf, pub state: idtx_behavior::ExperienceState }

impl BundleContext {
    #[must_use]
    pub fn new(root: PathBuf) -> Self { Self { root, state: idtx_behavior::ExperienceState::default() } }

    #[must_use]
    pub fn with_state(mut self, state: idtx_behavior::ExperienceState) -> Self { self.state = state; self }

    #[must_use]
    pub fn for_stage(stage: &Path) -> Self {
        if stage.to_str().is_some_and(|path| path.contains("://")) {
            return Self::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        }
        let parent = stage.parent().unwrap_or(Path::new("."));
        let root = parent.ancestors().find(|path| path.join("run.toml").is_file() || path.join("project.toml").is_file())
            .unwrap_or(parent).to_owned();
        Self::new(root)
    }

    pub fn install_asset_search_path(&self, extra: &[PathBuf]) -> Result<(), idtx_usd::StageError> {
        let mut roots = extra.to_vec();
        if !roots.contains(&self.root) { roots.push(self.root.clone()); }
        idtx_usd::set_default_asset_search_path(&roots)
    }

    #[must_use]
    pub fn load_input_map(&self, stage: &Path) -> InputMap {
        let local = stage.parent().unwrap_or(Path::new(".")).join("input_map.toml");
        let bundle = self.root.join("input_map.toml");
        for path in [local, bundle] {
            if let Ok(text) = std::fs::read_to_string(&path) {
                return InputMap::parse(&text).unwrap_or_else(|error| {
                    tracing::warn!(path = %path.display(), %error, "invalid input map");
                    InputMap::default()
                });
            }
        }
        InputMap::default()
    }

    /// The adapter installs its validated network/auth policy before this call.
    /// Hosts, clocks, queues and terminal lifecycle remain session-owned.
    pub fn configure_session(&self, session: &mut StudioSession) -> usize {
        session.set_editor_overlays_enabled(false);
        if !session.experience_state().is_fork() { session.set_experience_state(self.state.clone()); }
        session.set_behavior_limits(Limits::runtime());
        session.set_behavior_input_map(self.load_input_map(session.source_path()));
        if let Some(camera) = session.local_player_camera_path() { session.use_scene_camera(camera); }
        let directory = session.source_path().parent().unwrap_or(&self.root).to_owned();
        let attached = session.attach_authored_behaviors(move |asset| std::fs::read(directory.join(asset)).ok());
        session.set_behavior_playing(true);
        attached
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relocated_bundle_context_finds_its_own_root() {
        let root = std::env::temp_dir().join(format!("idtx-bundle-context-{}", std::process::id()));
        std::fs::create_dir_all(root.join("levels/room")).unwrap();
        std::fs::write(root.join("run.toml"), "").unwrap();
        let stage = root.join("levels/room/level.usda");
        assert_eq!(BundleContext::for_stage(&stage).root, root);
        std::fs::write(root.join("input_map.toml"), "[actions.bundle]\nkind = 'button'\nbindings = []\n").unwrap();
        std::fs::write(root.join("levels/room/input_map.toml"), "[actions.level]\nkind = 'button'\nbindings = []\n").unwrap();
        let moved = root.with_extension("relocated");
        std::fs::rename(&root, &moved).unwrap();
        let stage = moved.join("levels/room/level.usda");
        let context = BundleContext::for_stage(&stage);
        assert_eq!(context.root, moved);
        assert_ne!(context.load_input_map(&stage).resolve("level"), idtx_behavior::abi::InputActionId::UNRESOLVED);
        std::fs::remove_file(moved.join("levels/room/input_map.toml")).unwrap();
        assert_ne!(context.load_input_map(&stage).resolve("bundle"), idtx_behavior::abi::InputActionId::UNRESOLVED);
        let _ = std::fs::remove_dir_all(moved);
    }
}
