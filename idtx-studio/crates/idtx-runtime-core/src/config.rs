//! Runtime player configuration — the parameters an out-of-process preview
//! (or a packaged experience) is launched with.

use std::path::PathBuf;

use idtx_studio_session::TextureDecodeConfig;

/// How the player presents the stage.
///
/// The two modes drive the *same* frame loop; `Xr` additionally starts an
/// `OpenXR` session and renders stereo to the headset (with a desktop mirror),
/// while `Desktop` renders only the desktop view with the orbit camera. This
/// is the seam that lets a future editor offer a no-HMD "Preview (Desktop)"
/// without any further core work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunMode {
    /// Immersive `OpenXR` preview (real headset on the `pcvr` Windows build;
    /// falls back to [`RunMode::Desktop`] when XR is unavailable).
    #[default]
    Xr,
    /// Plain desktop window, orbit camera, no HMD.
    Desktop,
}

impl RunMode {
    /// Parse the `--mode` CLI value. Unknown values fall back to `Xr`.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "desktop" | "flat" | "2d" => Self::Desktop,
            _ => Self::Xr,
        }
    }
}

/// Everything the player needs to open and run a stage.
///
/// `Debug` is implemented by hand rather than derived so an injected
/// [`RunConfig::bearer_token`] is never echoed into logs (see `main.rs`, which
/// traces config fields at startup) — the token is redacted to `"<redacted>"`.
#[derive(Clone)]
pub struct RunConfig {
    /// Project/experience name used for the HTTP cache namespace. Seeded from
    /// `run.toml` for packaged runs and conductor previews.
    pub project_name: Option<String>,
    /// The USD stage to open (`.usda` / `.usdc` / `.usdz`).
    pub stage: PathBuf,
    /// Presentation mode.
    pub mode: RunMode,
    /// When set, render headlessly for exactly this many frames then exit.
    /// Used by tests and CI smoke checks. Unlike a windowed run, this needs no
    /// surface and each frame genuinely steps tick/animation/behaviors/xr.
    pub frames: Option<u64>,
    /// Initial window (or headless render-target) size in physical pixels.
    pub viewport: [u32; 2],
    /// A pre-obtained bearer token injected directly, bypassing the
    /// config/keychain/login path used for remote (`http(s)://`) stages. Set
    /// via `--token`. Takes precedence over [`RunConfig::token_file`].
    ///
    /// Passing a secret on the command line leaks it into `ps`/shell history;
    /// prefer [`RunConfig::token_file`] for anything but throwaway runs.
    pub bearer_token: Option<String>,
    /// Path to a file whose (trimmed) contents are the bearer token. Set via
    /// `--token-file`. Read lazily by [`RunConfig::resolved_token`] so the
    /// secret never appears in the process' argument vector.
    pub token_file: Option<PathBuf>,
    /// The controller device id to inject controller meshes for (e.g.
    /// `"questpro"`), set via `--controller-device`. When set it overrides the
    /// runtime's own live `OpenXR` probe — the editor detects the attached device
    /// first-hand and passes it through so an over-Link preview (where the
    /// second process degrades to `"generic"`) still injects the right models.
    /// When `None` the runtime probes the device itself, which is the path a
    /// packaged experience takes.
    pub controller_device: Option<String>,
    /// Absolute directories installed as the USD default-resolver search path
    /// before the stage opens, so project-relative asset references (a bare
    /// `@assets/foo.usdz@` that does not resolve against its anchoring layer)
    /// anchor at the project's asset roots — exactly as the editor does on
    /// project open. Empty for a hand-launched player with no manifest
    /// asset roots; the editor preview passes the open project's roots via
    /// `--asset-search-dir`, and a packaged bundle fills them from `run.toml`.
    pub asset_search_dirs: Vec<PathBuf>,
    /// Runtime-only state shared by boot, mask and incoming sessions.
    pub experience_state: idtx_behavior::ExperienceState,
    /// When `true`, a **desktop** run presents borderless-fullscreen and captures
    /// the mouse cursor on launch, with ESC quitting the app — the posture a
    /// packaged/cooked desktop player takes. When `false` (the default, and what
    /// an editor desktop preview uses) the run stays windowed and captures the
    /// cursor only on first click, releasing it on ESC. Ignored in XR mode and
    /// for headless runs. Resolved CLI (`--fullscreen` / `--windowed`) first,
    /// else `run.toml`'s `[experience] fullscreen`, else `false`.
    pub fullscreen: bool,
    /// Master switch for runtime USD assets, REST datasources and behavior HTTP.
    /// Deny-by-default (`false`): a `RestDatasource` prim only polls its endpoint
    /// when this is `true` **and** the host matches [`exec_allowed_hosts`]. Set via
    /// `--allow-network`, else `run.toml`'s `[network] allow`. See the
    /// `idtx-studio-session` `ExecNetworkPolicy` for the full SSRF rationale.
    ///
    /// [`exec_allowed_hosts`]: RunConfig::exec_allowed_hosts
    pub exec_allow_network: bool,
    /// Allowlist of REST datasource endpoint host patterns (exact or `*.domain`
    /// wildcards). An endpoint whose host matches none is blocked — listing is
    /// mandatory, so an empty list means no REST polling even when
    /// [`exec_allow_network`](RunConfig::exec_allow_network) is `true`. Seeded from
    /// `run.toml`'s `[network] allowed_hosts`, extended by `--allow-host`.
    pub exec_allowed_hosts: Vec<String>,
    /// Hosts explicitly allowed to use loopback/RFC1918/IPv6 unique-local
    /// addresses. Must also pass `exec_allowed_hosts`; metadata stays blocked.
    pub private_hosts: Vec<String>,
    /// Explicit credential-sharing origins, independent of network reachability.
    pub auth_origins: Vec<String>,
    /// Maximum accepted REST datasource response body size, in **megabytes**
    /// (MiB). Larger responses are dropped so a hostile/runaway endpoint cannot
    /// exhaust process memory. `0` (the default) resolves to the built-in default
    /// (1 MiB) — never unlimited. Seeded from `run.toml`'s `[network]
    /// max_response_mb`, overridable with `--max-response-mb`.
    pub exec_max_response_mb: u32,
    /// Streamed asset cap in MiB, independent of in-memory REST/behavior bodies.
    /// `0` uses 256 MiB. Set by `[network] max_asset_mb` / `--max-asset-mb`.
    pub max_asset_mb: u32,
    /// Per-instance behavior deferred-work byte budget override (`[behavior.limits]
    /// queue_bytes`). `0` uses the runtime default (2 MiB). Applied via
    /// [`RunConfig::behavior_limits`].
    pub behavior_queue_bytes: usize,
    /// `[behavior.limits] argument_bytes`; zero keeps the default 64 KiB.
    pub behavior_argument_bytes: usize,
    /// Per-request async-HTTP response cap + up-front reservation override
    /// (`[behavior.limits] http_response_bytes`). `0` uses the runtime default
    /// (1 MiB); also clamped by `[network] max_response_mb`.
    pub behavior_http_response_bytes: usize,
    /// Max concurrent in-flight async HTTP requests per behavior instance
    /// (`[behavior.limits] http_inflight`). `0` uses the runtime default (8).
    pub behavior_http_inflight: usize,
    /// ⚠️ **TEMPORARY / SECURITY RISK.** A bearer token stamped into the cooked
    /// `run.toml` (`[network] dev_bearer_token`) so a packaged **device** runtime
    /// (Quest / Vision Pro — no keychain, no `~/.idtx-studio/config.toml`) can
    /// authenticate remote (`http(s)://`) USD asset fetches referenced from an
    /// otherwise-local entry stage. Armed only for `[network] auth_origins`
    /// entry. Remove when the interactive logon-level flow lands — see
    /// `docs/plan_remote_refs_external_and_runtime_auth.md`. Seeded by
    /// [`crate::bootstrap::resolve_with`] from the manifest.
    pub dev_bearer_token: Option<String>,
    /// When set (via `--screenshot <path>`), a **headless** run reads the final
    /// rendered frame back from the GPU and writes it to this path as a PNG, then
    /// exits. Under `openexec` the capture waits for the compute graph to produce
    /// at least one value first, so the image reflects live data rather than the
    /// authored defaults. Requires the runtime be built with the `metrics` feature
    /// (the GPU readback, `StudioSession::read_pixels`, is metrics-gated); a
    /// non-metrics build logs an error instead of silently writing nothing.
    pub screenshot: Option<PathBuf>,
    /// Per-level assembly entries for a **multi-level** experience, in logical
    /// order, as absolute paths. Empty for a single-stage experience (which uses
    /// [`stage`](RunConfig::stage)). A transition target named by a behavior is
    /// resolved against this list. Seeded from `run.toml`'s `[experience].levels`
    /// by [`crate::bootstrap::resolve_with`], each entry anchored absolute against
    /// the bundle root.
    pub levels: Vec<PathBuf>,
    /// Prepared template registry with asset paths resolved during bootstrap.
    pub runtime_prototypes: Vec<(String, PathBuf, String)>,
    /// The per-level assembly the experience **starts on** (absolute). This is the
    /// level opened and played at boot and, for a per-level cooked bundle that
    /// declares no single `stage`, the path [`stage`](RunConfig::stage) itself
    /// opens. `None` for a single-stage experience. Seeded from
    /// `run.toml`'s `[experience].start_level`.
    pub start_level: Option<PathBuf>,
    /// The resident transfer/mask assembly rendered while the next level loads
    /// async (absolute). `None` means the swap is a **hard cut** (no mask scene,
    /// no resident transfer session). Seeded from `run.toml`'s
    /// `[experience].transfer_level`.
    pub transfer_level: Option<PathBuf>,
    /// Texture-decode profile the sessions open with — how material/controller
    /// textures are resolution-capped and mip-generated. Defaults to
    /// [`TextureDecodeConfig::FULL_RESOLUTION`] (unchanged behavior for a
    /// hand-launched run with no manifest); a cooked device bundle stamps a
    /// capped profile into `run.toml`'s `[experience].texture_quality`, which
    /// [`crate::bootstrap::resolve_with`] maps here. Capping bounds per-texture
    /// GPU residency so large (4K) textures do not blow the per-prefetch upload
    /// budget and fall back to a white null texture on a standalone headset.
    pub texture_quality: TextureDecodeConfig,
}

impl std::fmt::Debug for RunConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunConfig")
            .field("project_name", &self.project_name)
            .field("stage", &self.stage)
            .field("mode", &self.mode)
            .field("frames", &self.frames)
            .field("viewport", &self.viewport)
            .field(
                "bearer_token",
                &self.bearer_token.as_ref().map(|_| "<redacted>"),
            )
            .field("token_file", &self.token_file)
            .field("controller_device", &self.controller_device)
            .field("asset_search_dirs", &self.asset_search_dirs)
            .field("fullscreen", &self.fullscreen)
            .field("exec_allow_network", &self.exec_allow_network)
            .field("exec_allowed_hosts", &self.exec_allowed_hosts)
            .field("private_hosts", &self.private_hosts)
            .field("auth_origins", &self.auth_origins)
            .field("exec_max_response_mb", &self.exec_max_response_mb)
            .field("max_asset_mb", &self.max_asset_mb)
            .field("screenshot", &self.screenshot)
            .field("levels", &self.levels)
            .field("start_level", &self.start_level)
            .field("transfer_level", &self.transfer_level)
            .field("texture_quality", &self.texture_quality)
            .finish()
    }
}

impl RunConfig {
    /// A windowed run of `stage` in the given `mode`.
    #[must_use]
    pub fn new(stage: PathBuf, mode: RunMode) -> Self {
        Self {
            project_name: None,
            stage,
            mode,
            frames: None,
            viewport: [1280, 720],
            bearer_token: None,
            token_file: None,
            controller_device: None,
            asset_search_dirs: Vec::new(),
            experience_state: idtx_behavior::ExperienceState::default(),
            fullscreen: false,
            exec_allow_network: false,
            exec_allowed_hosts: Vec::new(),
            private_hosts: Vec::new(),
            auth_origins: Vec::new(),
            exec_max_response_mb: 0,
            max_asset_mb: 0,
            behavior_queue_bytes: 0,
            behavior_argument_bytes: 0,
            behavior_http_response_bytes: 0,
            behavior_http_inflight: 0,
            dev_bearer_token: None,
            screenshot: None,
            levels: Vec::new(),
            runtime_prototypes: Vec::new(),
            start_level: None,
            transfer_level: None,
            texture_quality: TextureDecodeConfig::FULL_RESOLUTION,
        }
    }

    /// `true` when this is a headless (surfaceless) fixed-frame run.
    #[must_use]
    pub fn is_headless(&self) -> bool {
        self.frames.is_some()
    }

    /// Behavior host limits for this run: [`idtx_behavior::Limits::runtime`] with
    /// any non-zero `[behavior.limits]` override applied. Each override is clamped
    /// to a sane ceiling so a bundle cannot request an absurd budget: `queue_bytes`
    /// to the whole-host byte ceiling, and the per-response reservation to the
    /// (possibly overridden) per-instance `queue_bytes` so at least one request can
    /// always be admitted.
    #[must_use]
    pub fn behavior_limits(&self) -> idtx_behavior::Limits {
        idtx_session_orchestration::runtime::behavior_limits(
            &idtx_runtime_manifest::BehaviorLimitsConfig {
                argument_bytes: self.behavior_argument_bytes,
                queue_bytes: self.behavior_queue_bytes,
                http_response_bytes: self.behavior_http_response_bytes,
                http_inflight: self.behavior_http_inflight,
            },
        )
    }

    #[cfg(feature = "remote")]
    pub(crate) fn egress_policy(&self) -> idtx_usd_resolver_http::NetworkPolicy {
        idtx_usd_resolver_http::NetworkPolicy::new(self.exec_allow_network, self.exec_allowed_hosts.clone())
            .with_private_hosts(self.private_hosts.clone())
            .with_max_response_mb(self.exec_max_response_mb)
    }

    #[cfg(feature = "remote")]
    pub(crate) fn asset_policy(&self) -> idtx_usd_resolver_http::NetworkPolicy {
        self.egress_policy().with_max_response_mb(if self.max_asset_mb == 0 {
            idtx_runtime_manifest::DEFAULT_MAX_ASSET_MB
        } else {
            self.max_asset_mb
        })
    }

    /// Resolve an explicitly-injected bearer token, if any.
    ///
    /// [`RunConfig::bearer_token`] wins over [`RunConfig::token_file`]; when only
    /// a token file is set it is read and trimmed. Returns `Ok(None)` when
    /// neither is set (the caller then falls back to the config/keychain/login
    /// path). Reading an unreadable/empty token file is an error — a user who
    /// explicitly pointed at a token file should hear about it rather than
    /// silently fall through to keychain auth.
    ///
    /// # Errors
    ///
    /// Returns the read error when [`RunConfig::token_file`] cannot be read, or
    /// a message when the file is empty/whitespace-only.
    pub fn resolved_token(&self) -> Result<Option<String>, String> {
        if let Some(token) = &self.bearer_token {
            return Ok(Some(token.clone()));
        }
        if let Some(path) = &self.token_file {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| format!("could not read --token-file {}: {e}", path.display()))?;
            let token = raw.trim();
            if token.is_empty() {
                return Err(format!("--token-file {} is empty", path.display()));
            }
            return Ok(Some(token.to_string()));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mode_case_insensitively_with_xr_default() {
        assert_eq!(RunMode::parse("desktop"), RunMode::Desktop);
        assert_eq!(RunMode::parse("Desktop"), RunMode::Desktop);
        assert_eq!(RunMode::parse("flat"), RunMode::Desktop);
        assert_eq!(RunMode::parse("xr"), RunMode::Xr);
        assert_eq!(RunMode::parse("anything-else"), RunMode::Xr);
        assert_eq!(RunMode::default(), RunMode::Xr);
    }

    #[test]
    fn headless_flag_tracks_frames() {
        let mut cfg = RunConfig::new(PathBuf::from("s.usda"), RunMode::Desktop);
        assert!(!cfg.is_headless());
        cfg.frames = Some(10);
        assert!(cfg.is_headless());
    }

    #[test]
    fn new_defaults_auth_fields_to_none() {
        let cfg = RunConfig::new(PathBuf::from("s.usda"), RunMode::Xr);
        assert!(cfg.bearer_token.is_none());
        assert!(cfg.token_file.is_none());
        assert!(cfg.controller_device.is_none());
        assert!(cfg.asset_search_dirs.is_empty());
        assert!(!cfg.fullscreen);
        assert!(!cfg.exec_allow_network);
        assert!(cfg.exec_allowed_hosts.is_empty());
        assert_eq!(cfg.resolved_token(), Ok(None));
    }

    #[test]
    fn resolved_token_prefers_explicit_bearer_token() {
        let mut cfg = RunConfig::new(PathBuf::from("s.usda"), RunMode::Xr);
        cfg.bearer_token = Some("abc123".to_string());
        // Even with a (would-be-unreadable) token_file set, the explicit token wins.
        cfg.token_file = Some(PathBuf::from("/nonexistent/does-not-exist.token"));
        assert_eq!(cfg.resolved_token(), Ok(Some("abc123".to_string())));
    }

    #[test]
    fn resolved_token_reads_and_trims_token_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("idtx-runtime-token-{}.txt", std::process::id()));
        std::fs::write(&path, "  file-token-xyz\n").unwrap();
        let mut cfg = RunConfig::new(PathBuf::from("s.usda"), RunMode::Xr);
        cfg.token_file = Some(path.clone());
        assert_eq!(cfg.resolved_token(), Ok(Some("file-token-xyz".to_string())));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn resolved_token_errors_on_missing_token_file() {
        let mut cfg = RunConfig::new(PathBuf::from("s.usda"), RunMode::Xr);
        cfg.token_file = Some(PathBuf::from("/nonexistent/does-not-exist.token"));
        assert!(cfg.resolved_token().is_err());
    }

    #[test]
    fn behavior_limits_apply_overrides_and_clamp() {
        let defaults = idtx_behavior::Limits::runtime().work;
        // No overrides → runtime defaults unchanged.
        let cfg = RunConfig::new(PathBuf::from("s.usda"), RunMode::Desktop);
        let work = cfg.behavior_limits().work;
        assert_eq!(work.queue_bytes, defaults.queue_bytes);
        assert_eq!(work.http_response_bytes, defaults.http_response_bytes);
        assert_eq!(work.http_inflight_max, defaults.http_inflight_max);

        // Non-zero overrides are applied.
        let mut cfg = RunConfig::new(PathBuf::from("s.usda"), RunMode::Desktop);
        cfg.behavior_queue_bytes = 4 * 1024 * 1024;
        cfg.behavior_http_response_bytes = 64 * 1024;
        cfg.behavior_http_inflight = 4;
        let work = cfg.behavior_limits().work;
        assert_eq!(work.queue_bytes, 4 * 1024 * 1024);
        assert_eq!(work.http_response_bytes, 64 * 1024);
        assert_eq!(work.http_inflight_max, 4);

        // queue_bytes is clamped to the whole-host ceiling; the per-response
        // reservation is clamped to the (overridden) queue budget.
        let mut cfg = RunConfig::new(PathBuf::from("s.usda"), RunMode::Desktop);
        cfg.behavior_queue_bytes = usize::MAX;
        cfg.behavior_http_response_bytes = usize::MAX;
        let work = cfg.behavior_limits().work;
        assert_eq!(work.queue_bytes, defaults.host_queue_bytes);
        assert_eq!(work.http_response_bytes, work.queue_bytes);
    }
}
