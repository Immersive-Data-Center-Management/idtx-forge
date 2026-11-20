//! Shared bundle/session bootstrap for every runtime adapter.
use idtx_behavior::{InputMap, Limits};
use idtx_studio_session::StudioSession;
use std::path::{Path, PathBuf};

/// Winit-free conversion shared by desktop, Quest and visionOS. Zero means
/// "use the runtime default". Reservations always fit the effective queue,
/// including when only the queue budget is reduced in the manifest.
#[must_use]
pub fn behavior_limits(config: &idtx_runtime_manifest::BehaviorLimitsConfig) -> Limits {
    let mut limits = Limits::runtime();
    let work = &mut limits.work;
    if config.queue_bytes != 0 {
        work.queue_bytes = config.queue_bytes.min(work.host_queue_bytes);
    }
    if config.http_response_bytes != 0 {
        work.http_response_bytes = config.http_response_bytes;
    }
    work.http_response_bytes = work.http_response_bytes.min(work.queue_bytes);
    if config.http_inflight != 0 {
        work.http_inflight_max = config.http_inflight;
    }
    limits
}

#[cfg(feature = "metrics")]
pub(crate) fn peak_rss_bytes() -> u64 {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: getrusage initializes the correctly sized output on success.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0 {
            // SAFETY: successful getrusage initialized the output.
            let rss = u64::try_from(unsafe { usage.assume_init() }.ru_maxrss).unwrap_or(0);
            return if cfg!(target_vendor = "apple") {
                rss
            } else {
                rss.saturating_mul(1024)
            };
        }
    }
    0
}

#[derive(Debug, Clone)]
pub struct BundleContext {
    pub root: PathBuf,
    pub state: idtx_behavior::ExperienceState,
    pub runtime_prototypes: Vec<(String, PathBuf, String)>,
    pub behavior_limits: Option<Limits>,
}

impl BundleContext {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            state: idtx_behavior::ExperienceState::default(),
            runtime_prototypes: Vec::new(),
            behavior_limits: None,
        }
    }

    #[must_use]
    pub fn with_state(mut self, state: idtx_behavior::ExperienceState) -> Self {
        self.state = state;
        self
    }

    /// Override the behavior host limits applied in [`Self::configure_session`].
    /// `None` (the default) uses [`Limits::runtime`]; the runtime adapter passes a
    /// value derived from the manifest's `[behavior.limits]` here.
    #[must_use]
    pub fn with_behavior_limits(mut self, limits: Limits) -> Self {
        self.behavior_limits = Some(limits);
        self
    }

    #[must_use]
    pub fn for_stage(stage: &Path) -> Self {
        if stage.to_str().is_some_and(|path| path.contains("://")) {
            return Self::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        }
        let parent = stage.parent().unwrap_or(Path::new("."));
        let root = parent
            .ancestors()
            .find(|path| path.join("run.toml").is_file() || path.join("project.toml").is_file())
            .unwrap_or(parent)
            .to_owned();
        Self::new(root)
    }

    #[must_use]
    pub fn with_runtime_prototypes(mut self, prototypes: Vec<(String, PathBuf, String)>) -> Self {
        self.runtime_prototypes = prototypes;
        self
    }

    pub fn install_asset_search_path(&self, extra: &[PathBuf]) -> Result<(), idtx_usd::StageError> {
        let mut roots = extra.to_vec();
        if !roots.contains(&self.root) {
            roots.push(self.root.clone());
        }
        idtx_usd::set_default_asset_search_path(&roots)
    }

    #[must_use]
    pub fn load_input_map(&self, stage: &Path) -> InputMap {
        let local = stage
            .parent()
            .unwrap_or(Path::new("."))
            .join("input_map.toml");
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
        session.set_runtime_prototypes(self.runtime_prototypes.clone());
        session.set_editor_overlays_enabled(false);
        if !session.experience_state().is_fork() {
            session.set_experience_state(self.state.clone());
        }
        session.set_behavior_limits(self.behavior_limits.unwrap_or_else(Limits::runtime));
        session.set_behavior_input_map(self.load_input_map(session.source_path()));
        if let Some(camera) = session.local_player_camera_path() {
            session.use_scene_camera(camera);
        }
        let directory = session
            .source_path()
            .parent()
            .unwrap_or(&self.root)
            .to_owned();
        let attached = session
            .attach_authored_behaviors(move |asset| std::fs::read(directory.join(asset)).ok());
        session.set_behavior_playing(true);
        attached
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires OpenUSD link and headless GPU"]
    fn boot_mask_and_incoming_apply_manifest_queue_limits_before_attachment() {
        idtx_usd_extension::install().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("level.usda");
        std::fs::write(
            &stage,
            r#"#usda 1.0
(defaultPrim = "World")
def Xform "World" {
    def Xform "Behavior" (prepend apiSchemas = ["IdtxBehaviorAPI"]) {
        token idtx:behavior:kind = "runtime"
        string idtx:behavior:name = "budget"
        string idtx:behavior:uuid = "budget"
        string idtx:behavior:version = "1"
        asset idtx:behavior:module = @budget.wasm@
        token[] idtx:behavior:capabilities = ["stage_edit"]
    }
}
"#,
        )
        .unwrap();
        // A 512-byte edit must be rejected by the 64-byte instance queue even
        // during on_attach. Using runtime defaults would admit it and trap here.
        let wasm = wat::parse_str(format!(r#"(module
            (import "idtx" "idtx_entity_self" (func $self (result i64)))
            (import "idtx" "idtx_session_set_widget_value" (func $value (param i64 i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "Value")
            (data (i32.const 64) "{}")
            (func (export "_idtx_abi_version") (result i32) i32.const 3)
            (func (export "_idtx_on_attach")
                (if (i32.ne (call $value (call $self) (i32.const 0) (i32.const 5) (i32.const 64) (i32.const 512)) (i32.const 6))
                    (then unreachable))))"#, "x".repeat(512))).unwrap();
        std::fs::write(temp.path().join("budget.wasm"), wasm).unwrap();
        let config = idtx_runtime_manifest::BehaviorLimitsConfig {
            queue_bytes: 64,
            ..Default::default()
        };
        let boot = BundleContext::new(temp.path().to_owned())
            .with_behavior_limits(behavior_limits(&config));
        let (device, queue) = crate::headless_device().expect("headless GPU required");
        for (phase, context) in [
            ("boot", boot.clone()),
            ("mask", boot.clone()),
            ("incoming", boot),
        ] {
            let mut session = StudioSession::open_with_gpu(
                &stage,
                device.clone(),
                queue.clone(),
                [64, 64],
                idtx_studio_session::TextureDecodeConfig::default(),
                idtx_studio_session::RenderWorldUnitScale::METRE,
            )
            .unwrap();
            assert_eq!(context.configure_session(&mut session), 1, "{phase}");
            session.tick_behaviors(0.01);
            session.validate_runtime_readiness().unwrap();
        }
        // Negative control: the same guest detects a missing override.
        let mut session = StudioSession::open_with_gpu(
            &stage,
            device,
            queue,
            [64, 64],
            idtx_studio_session::TextureDecodeConfig::default(),
            idtx_studio_session::RenderWorldUnitScale::METRE,
        )
        .unwrap();
        BundleContext::new(temp.path().to_owned()).configure_session(&mut session);
        session.tick_behaviors(0.01);
        assert!(session.validate_runtime_readiness().is_err());
    }

    #[test]
    fn manifest_limits_preserve_defaults_and_bound_reservations() {
        use idtx_runtime_manifest::BehaviorLimitsConfig;
        let defaults = Limits::runtime();
        let unchanged = behavior_limits(&BehaviorLimitsConfig::default());
        assert_eq!(unchanged.work.queue_bytes, defaults.work.queue_bytes);
        assert_eq!(
            unchanged.work.http_response_bytes,
            defaults.work.http_response_bytes
        );
        assert_eq!(
            unchanged.work.http_inflight_max,
            defaults.work.http_inflight_max
        );
        for config in [
            BehaviorLimitsConfig {
                queue_bytes: 64,
                http_response_bytes: 0,
                http_inflight: 1,
            },
            BehaviorLimitsConfig {
                queue_bytes: 4096,
                http_response_bytes: 512,
                http_inflight: 4,
            },
            BehaviorLimitsConfig {
                queue_bytes: usize::MAX,
                http_response_bytes: usize::MAX,
                http_inflight: 8,
            },
        ] {
            let limits = behavior_limits(&config);
            assert_eq!(
                limits.work.queue_bytes,
                config.queue_bytes.min(defaults.work.host_queue_bytes)
            );
            assert_eq!(
                limits.work.http_response_bytes,
                (if config.http_response_bytes == 0 {
                    defaults.work.http_response_bytes
                } else {
                    config.http_response_bytes
                })
                .min(limits.work.queue_bytes)
            );
            assert_eq!(limits.work.http_inflight_max, config.http_inflight);
            assert_eq!(limits.memory_cap, defaults.memory_cap);
            assert_eq!(limits.startup_fuel, defaults.startup_fuel);
            assert_eq!(limits.hook_fuel, defaults.hook_fuel);
            let boot = BundleContext::new(PathBuf::from("bundle")).with_behavior_limits(limits);
            // The mask/incoming configuration closures retain cloned contexts.
            for context in [boot.clone(), boot.clone(), boot] {
                assert_eq!(
                    context.behavior_limits.unwrap().work.queue_bytes,
                    limits.work.queue_bytes
                );
            }
        }
    }

    #[test]
    fn relocated_bundle_context_finds_its_own_root() {
        let root = std::env::temp_dir().join(format!("idtx-bundle-context-{}", std::process::id()));
        std::fs::create_dir_all(root.join("levels/room")).unwrap();
        std::fs::write(root.join("run.toml"), "").unwrap();
        let stage = root.join("levels/room/level.usda");
        assert_eq!(BundleContext::for_stage(&stage).root, root);
        std::fs::write(
            root.join("input_map.toml"),
            "[actions.bundle]\nkind = 'button'\nbindings = []\n",
        )
        .unwrap();
        std::fs::write(
            root.join("levels/room/input_map.toml"),
            "[actions.level]\nkind = 'button'\nbindings = []\n",
        )
        .unwrap();
        let moved = root.with_extension("relocated");
        std::fs::rename(&root, &moved).unwrap();
        let stage = moved.join("levels/room/level.usda");
        let context = BundleContext::for_stage(&stage);
        assert_eq!(context.root, moved);
        assert_ne!(
            context.load_input_map(&stage).resolve("level"),
            idtx_behavior::abi::InputActionId::UNRESOLVED
        );
        std::fs::remove_file(moved.join("levels/room/input_map.toml")).unwrap();
        assert_ne!(
            context.load_input_map(&stage).resolve("bundle"),
            idtx_behavior::abi::InputActionId::UNRESOLVED
        );
        let _ = std::fs::remove_dir_all(moved);
    }
}
