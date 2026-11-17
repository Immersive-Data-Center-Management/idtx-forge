//! Packaged-experience bootstrap: layer CLI arguments over a discovered
//! `run.toml` manifest to produce the effective [`RunConfig`].
//!
//! This is the runtime-consumption seam. A packaged bundle ships a
//! [`RuntimeManifest`] (`run.toml`) next to the runtime binary; its
//! `[experience]` section names the stage to open and the default mode, and its
//! `[pins]` section records the behavior ABI the bundle was cooked against. A
//! hand-launched player still passes `--stage`/`--mode` explicitly, so CLI
//! values always win — the manifest only fills what the CLI leaves unset.

use std::path::{Path, PathBuf};

use idtx_runtime_manifest::RuntimeManifest;

use crate::config::{RunConfig, RunMode};

/// Resolve the effective [`RunConfig`] for a packaged or hand-launched run.
///
/// Discovers a `run.toml` (via [`RuntimeManifest::discover`]) and layers the
/// CLI over it: see [`resolve_with`] for the precedence rules. The manifest
/// stage, when relative, is resolved against the directory holding the runtime
/// binary (the packaged bundle root).
///
/// # Errors
/// Returns a message when neither the CLI nor the manifest names a stage.
pub fn resolve_run_config(
    cli_stage: Option<PathBuf>,
    cli_mode: Option<RunMode>,
    cli_fullscreen: Option<bool>,
) -> Result<RunConfig, String> {
    let load = || {
        let manifest = RuntimeManifest::discover();
        let bundle_root = std::env::var_os(idtx_runtime_manifest::ENV_MANIFEST)
            .map(PathBuf::from)
            .and_then(|path| {
                let absolute = if path.is_absolute() { path } else { std::env::current_dir().ok()?.join(path) };
                absolute.parent().map(Path::to_path_buf)
            })
            .or_else(|| std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)));
        resolve_with(&manifest, cli_stage, cli_mode, cli_fullscreen, bundle_root.as_deref())
    };
    #[cfg(feature = "remote")]
    { idtx_usd_resolver_http::configure_network(load, RunConfig::asset_policy) }
    #[cfg(not(feature = "remote"))]
    { load() }
}

/// Pure core of [`resolve_run_config`], factored out so the precedence rules are
/// unit-testable without touching the process' real executable path or a
/// `run.toml` on disk.
///
/// Precedence:
/// * **stage** — `cli_stage` if given, else the manifest's `[experience].stage`
///   (resolved against `bundle_root` when relative), else the manifest's
///   `[experience].start_level` (a per-level cooked bundle emits `start_level`
///   and the per-level assembly paths but no single `stage`), else an error.
/// * **mode** — `cli_mode` if given, else the manifest's `[experience].mode`
///   (parsed), else [`RunMode::default`].
/// * **fullscreen** — `cli_fullscreen` if given, else the manifest's
///   `[experience].fullscreen`, else `false` (windowed).
/// * **levels / start_level / transfer_level** — copied from the manifest's
///   `[experience]` section, each relative path anchored absolute against
///   `bundle_root` exactly like `stage`. Empty / `None` for a single-stage
///   bundle.
///
/// Also warns (via `tracing`) when the manifest's recorded behavior ABI pin does
/// not match the host ABI — the documented silent-fallback failure mode where
/// bundled behaviors quietly no-op unless rebuilt.
///
/// # Errors
/// Returns a message when none of `cli_stage`, the manifest's stage, or the
/// manifest's start_level names a stage.
pub fn resolve_with(
    manifest: &RuntimeManifest,
    cli_stage: Option<PathBuf>,
    cli_mode: Option<RunMode>,
    cli_fullscreen: Option<bool>,
    bundle_root: Option<&Path>,
) -> Result<RunConfig, String> {
    if manifest.abi_matches(idtx_behavior::ABI_VERSION) == Some(false) {
        tracing::warn!(
            host_abi = idtx_behavior::ABI_VERSION,
            "packaged behavior ABI pin does not match this runtime; bundled behaviors \
             will silently no-op until rebuilt against the current ABI"
        );
    }

    let (exp_stage, exp_mode) = manifest.experience_run_defaults();

    // Anchor a manifest-relative path against the bundle root (an absolute path
    // is taken verbatim) — the same rule the single `stage` uses.
    let anchor = |rel: &Path| -> PathBuf {
        match bundle_root {
            Some(root) if rel.is_relative() => root.join(rel),
            _ => rel.to_path_buf(),
        }
    };

    let exp_start_level = manifest.experience.start_level.as_deref();

    let stage = if let Some(s) = cli_stage {
        s
    } else if let Some(rel) = exp_stage {
        anchor(rel)
    } else if let Some(rel) = exp_start_level {
        // Per-level cooked bundle: it emits `start_level` + the per-level assembly
        // paths but no single `stage`, so boot on the resolved start level.
        anchor(rel)
    } else {
        return Err(
            "no stage given (pass --stage <path>) and run.toml declares no [experience].stage"
                .to_owned(),
        );
    };

    let mode = cli_mode
        .or_else(|| exp_mode.map(RunMode::parse))
        .unwrap_or_default();

    let mut cfg = RunConfig::new(stage, mode);
    for (id, prototype) in &manifest.runtime_prototypes {
        prototype.validate(id).map_err(|error| format!("prototype {id}: {error}"))?;
    }
    cfg.runtime_prototypes = manifest.resolved_runtime_prototypes(bundle_root.unwrap_or(Path::new(".")));
    // Fullscreen posture: CLI flag wins, else the manifest's packaged default,
    // else windowed. A cooked desktop bundle stamps `fullscreen = true`; a preview
    // launch passes neither and reads no manifest, so it stays windowed.
    cfg.fullscreen = cli_fullscreen
        .or_else(|| manifest.experience_fullscreen_default())
        .unwrap_or(false);
    // Seed the USD resolver search path from the manifest's asset roots, resolved
    // to absolute against the bundle root (a packaged `assets/` dir, or configured
    // roots). A preview launch additionally passes the open project's roots via
    // `--asset-search-dir` (merged in `main`), which take precedence.
    cfg.asset_search_dirs = manifest
        .asset_search_roots()
        .into_iter()
        .map(|root| match bundle_root {
            Some(base) if root.is_relative() => base.join(root),
            _ => root,
        })
        .collect();
    // Stage-authored REST datasource network capability (deny-by-default). Seeded
    // from the manifest's `[network]` table; the CLI (`--allow-network` /
    // `--allow-host`) merges over this in `main`.
    let (allow_network, allowed_hosts) = manifest.network_policy();
    cfg.exec_allow_network = allow_network;
    cfg.exec_allowed_hosts = allowed_hosts;
    cfg.private_hosts = manifest.network.private_hosts.clone();
    cfg.auth_origins = manifest.network.auth_origins.clone();
    cfg.exec_max_response_mb = manifest.network_max_response_mb();
    cfg.max_asset_mb = manifest.network.max_asset_mb;
    // Per-instance behavior work-budget overrides (`[behavior.limits]`). Zero
    // leaves the runtime default in place; a non-zero value is applied (and
    // clamped) by `RunConfig::behavior_limits`.
    let behavior_limits = manifest.behavior_limits();
    cfg.behavior_queue_bytes = behavior_limits.queue_bytes;
    cfg.behavior_http_response_bytes = behavior_limits.http_response_bytes;
    cfg.behavior_http_inflight = behavior_limits.http_inflight;
    // TEMPORARY (see docs/plan_remote_refs_external_and_runtime_auth.md): carry
    // the opt-in stamped dev bearer token through so a device runtime can arm it
    // for remote USD fetches. Remove with the interactive logon-level flow.
    cfg.dev_bearer_token = manifest.network_dev_bearer_token().map(str::to_owned);
    // Per-level assembly entries for a multi-level experience, anchored absolute
    // against the bundle root exactly like `stage`. Empty / None leaves the
    // runtime in single-stage mode (eager open + play, no transfer session).
    cfg.levels = manifest
        .experience
        .levels
        .iter()
        .map(|p| anchor(p.as_path()))
        .collect();
    cfg.start_level = exp_start_level.map(anchor);
    cfg.transfer_level = manifest
        .experience
        .transfer_level
        .as_deref()
        .map(anchor);
    // Texture-decode profile: the manifest's stamped token (device cooks stamp a
    // capped profile), else the RunConfig default (FULL_RESOLUTION). An
    // unrecognized token falls back to the default rather than failing the run.
    if let Some(token) = manifest.experience.texture_quality.as_deref() {
        if let Some(profile) = idtx_studio_session::TextureDecodeConfig::from_profile_token(token) {
            cfg.texture_quality = profile;
        } else {
            tracing::warn!(
                token,
                "run.toml [experience].texture_quality is not a known profile \
                 (full_resolution/authoring/inspector); using the default"
            );
        }
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prototypes_use_the_manifest_anchor_and_survive_transition_config_cloning() {
        let mut manifest = RuntimeManifest::default();
        manifest.runtime_prototypes.insert("panel".into(), idtx_runtime_manifest::RuntimePrototypeConfig {
            asset: "runtime_prototypes/panel.usdz".into(), prim: "/Panel".into(),
        });
        let root = Path::new("C:/Bundle");
        let cfg = resolve_with(&manifest, Some("level.usda".into()), None, None, Some(root)).unwrap();
        assert_eq!(cfg.runtime_prototypes, vec![("panel".into(), root.join("runtime_prototypes/panel.usdz"), "/Panel".into())]);
        assert_eq!(cfg.clone().runtime_prototypes, cfg.runtime_prototypes);
    }

    use idtx_runtime_manifest::ExperienceConfig;

    fn manifest_with(stage: Option<&str>, mode: Option<&str>) -> RuntimeManifest {
        RuntimeManifest::default().with_experience(ExperienceConfig {
            stage: stage.map(PathBuf::from),
            mode: mode.map(str::to_string),
            name: None,
            target: None,
            fullscreen: None,
            ..Default::default()
        })
    }

    #[test]
    fn cli_stage_and_mode_win_over_manifest() {
        let m = manifest_with(Some("packaged.usdz"), Some("xr"));
        let cfg = resolve_with(
            &m,
            Some(PathBuf::from("cli.usda")),
            Some(RunMode::Desktop),
            None,
            Some(Path::new("/bundle")),
        )
        .unwrap();
        assert_eq!(cfg.stage, PathBuf::from("cli.usda"));
        assert_eq!(cfg.mode, RunMode::Desktop);
    }

    #[test]
    fn manifest_fills_stage_and_mode_when_cli_absent() {
        let m = manifest_with(Some("packaged.usdz"), Some("desktop"));
        let cfg = resolve_with(&m, None, None, None, Some(Path::new("/bundle"))).unwrap();
        // Relative manifest stage is resolved against the bundle root.
        assert_eq!(cfg.stage, Path::new("/bundle").join("packaged.usdz"));
        assert_eq!(cfg.mode, RunMode::Desktop);
    }

    #[test]
    fn absolute_manifest_stage_is_not_rejoined() {
        let abs = if cfg!(windows) {
            "C:\\stages\\a.usdz"
        } else {
            "/stages/a.usdz"
        };
        let m = manifest_with(Some(abs), None);
        let cfg = resolve_with(&m, None, None, None, Some(Path::new("/bundle"))).unwrap();
        assert_eq!(cfg.stage, PathBuf::from(abs));
        // No manifest mode + no CLI mode → default.
        assert_eq!(cfg.mode, RunMode::default());
    }

    #[test]
    fn no_stage_anywhere_is_an_error() {
        let m = RuntimeManifest::default();
        assert!(resolve_with(&m, None, None, None, Some(Path::new("/bundle"))).is_err());
    }

    #[test]
    fn cli_mode_wins_when_manifest_has_no_mode() {
        let m = manifest_with(Some("packaged.usdz"), None);
        let cfg =
            resolve_with(&m, None, Some(RunMode::Desktop), None, Some(Path::new("/b"))).unwrap();
        assert_eq!(cfg.mode, RunMode::Desktop);
    }

    #[test]
    fn fullscreen_defaults_to_windowed_when_unset_everywhere() {
        let m = manifest_with(Some("packaged.usdz"), Some("desktop"));
        let cfg = resolve_with(&m, None, None, None, Some(Path::new("/b"))).unwrap();
        assert!(!cfg.fullscreen);
    }

    #[test]
    fn manifest_fullscreen_used_when_cli_absent() {
        let mut m = manifest_with(Some("packaged.usdz"), Some("desktop"));
        m.experience.fullscreen = Some(true);
        let cfg = resolve_with(&m, None, None, None, Some(Path::new("/b"))).unwrap();
        assert!(cfg.fullscreen);
    }

    #[test]
    fn cli_windowed_overrides_manifest_fullscreen() {
        let mut m = manifest_with(Some("packaged.usdz"), Some("desktop"));
        m.experience.fullscreen = Some(true);
        // Explicit `--windowed` (Some(false)) beats the manifest's fullscreen=true.
        let cfg = resolve_with(&m, None, None, Some(false), Some(Path::new("/b"))).unwrap();
        assert!(!cfg.fullscreen);
    }

    #[test]
    fn cli_fullscreen_overrides_manifest_windowed() {
        let m = manifest_with(Some("packaged.usdz"), Some("desktop"));
        let cfg = resolve_with(&m, None, None, Some(true), Some(Path::new("/b"))).unwrap();
        assert!(cfg.fullscreen);
    }

    #[test]
    fn relative_manifest_asset_roots_resolve_against_bundle_root() {
        use idtx_runtime_manifest::AssetConfig;
        let m = RuntimeManifest {
            assets: AssetConfig {
                search_paths: vec![PathBuf::from("assets")],
            },
            ..manifest_with(Some("packaged.usdz"), Some("desktop"))
        };
        let cfg = resolve_with(&m, None, None, None, Some(Path::new("/bundle"))).unwrap();
        // The configured relative root is anchored at the bundle root, so the
        // runtime installs it as an absolute USD resolver search dir.
        assert!(
            cfg.asset_search_dirs
                .contains(&PathBuf::from("/bundle").join("assets")),
            "expected bundle-anchored asset root, got {:?}",
            cfg.asset_search_dirs
        );
    }

    #[test]
    fn per_level_manifest_boots_start_level_and_carries_levels() {
        // A per-level cooked bundle emits start/transfer/levels PATHS but no single
        // `stage`; boot must fall back to start_level as the opened stage and carry
        // the level table + transfer path, all anchored against the bundle root.
        let m = RuntimeManifest::default().with_experience(ExperienceConfig {
            stage: None,
            levels: vec![
                PathBuf::from("levels/forest/level.usda"),
                PathBuf::from("levels/cave/level.usda"),
            ],
            start_level: Some(PathBuf::from("levels/forest/level.usda")),
            transfer_level: Some(PathBuf::from("transfer/level.usda")),
            ..Default::default()
        });
        let cfg = resolve_with(&m, None, None, None, Some(Path::new("/bundle"))).unwrap();
        let root = Path::new("/bundle");
        assert_eq!(cfg.stage, root.join("levels/forest/level.usda"));
        assert_eq!(cfg.start_level, Some(root.join("levels/forest/level.usda")));
        assert_eq!(cfg.transfer_level, Some(root.join("transfer/level.usda")));
        assert_eq!(
            cfg.levels,
            vec![
                root.join("levels/forest/level.usda"),
                root.join("levels/cave/level.usda"),
            ]
        );
    }

    #[test]
    fn single_stage_manifest_leaves_level_fields_empty() {
        let m = manifest_with(Some("packaged.usdz"), Some("desktop"));
        let cfg = resolve_with(&m, None, None, None, Some(Path::new("/bundle"))).unwrap();
        assert_eq!(cfg.stage, Path::new("/bundle").join("packaged.usdz"));
        assert!(cfg.levels.is_empty());
        assert!(cfg.start_level.is_none());
        assert!(cfg.transfer_level.is_none());
    }

    #[cfg(feature = "remote")]
    #[test]
    fn manifest_network_contract_reaches_both_runtime_consumers() {
        let mut manifest = manifest_with(Some("stage.usda"), None);
        manifest.network.allow = true;
        manifest.network.allowed_hosts = vec!["127.0.0.1".into()];
        manifest.network.private_hosts = vec!["127.0.0.1".into()];
        manifest.network.max_response_mb = 2;
        manifest.network.max_asset_mb = 64;
        let cfg = resolve_with(&manifest, None, None, None, None).unwrap();
        assert_eq!(cfg.private_hosts, ["127.0.0.1"]);
        assert_eq!(cfg.egress_policy().max_response_bytes(), manifest.network.egress_policy().max_response_bytes());
        assert_eq!(cfg.asset_policy().max_response_bytes(), manifest.network.asset_policy().max_response_bytes());
        manifest.network.allow = false;
        let cfg = resolve_with(&manifest, None, None, None, None).unwrap();
        assert!(cfg.egress_policy().is_denied());
        assert!(cfg.asset_policy().is_denied());
    }
}
