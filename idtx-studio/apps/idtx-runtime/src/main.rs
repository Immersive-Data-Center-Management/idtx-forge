//! `idtx-runtime` — the packaged desktop/XR player.
//!
//! Opens a USD stage and runs it under the shared runtime core: behaviors
//! execute, the scene renders **without** editor chrome, and (on the `pcvr`
//! Windows build) an `OpenXR` session drives a connected headset. This is the
//! binary the editor spawns for "Preview in Runtime," simulating a packaged
//! experience for this target.
//!
//! CLI:
//! ```text
//! idtx-runtime [--stage] <path.usd[a|c|z]> [--mode xr|desktop] [--headless] [--frames N]
//!              [--token <bearer>] [--token-file <path>] [--controller-device <id>]
//!              [--asset-search-dir <dir>]... [--allow-network] [--allow-host <pattern>]...
//!              [--screenshot <path.png>]
//! ```
//!
//! `--screenshot <path>` runs headlessly, captures the rendered frame to a PNG,
//! then exits (implies `--headless`). Under `openexec` the capture first waits
//! for the compute graph to produce at least one value, so the image shows live
//! data. Requires the runtime be built with the `metrics` feature (the GPU
//! readback is metrics-gated).
//!
//! For a remote (`http(s)://`) stage, `--token` / `--token-file` inject a bearer
//! token directly, short-circuiting the shared-config/keychain/login path.
//! `--token-file` is preferred: `--token` leaks the secret into `ps` and shell
//! history.
//!
//! `--asset-search-dir <dir>` (repeatable) adds an absolute directory to the USD
//! default-resolver search path so project-relative asset references
//! (`@assets/foo.usdz@`) resolve. The editor preview passes the open project's
//! asset roots so a spawned runtime resolves references exactly as the editor
//! does; CLI dirs take precedence over any `run.toml`-derived roots.
//!
//! `--controller-device <id>` (XR) names the attached controller device (e.g.
//! `questpro`) so the runtime injects the matching controller meshes. The editor
//! probes the device first-hand and passes it through; without the flag the
//! runtime falls back to its own live `OpenXR` probe (the packaged-experience
//! path).
//!
//! `--allow-network` enables stage-authored `OpenExec` REST datasources (off by
//! default), and `--allow-host <pattern>` (repeatable) adds an allowlisted
//! endpoint host (exact `api.example.com` or `*.example.com` wildcard). Both
//! merge over a `run.toml` `[network]` section **unless** `--replace-network-policy`
//! is also present, in which case the CLI values *replace* the `[network]` section
//! entirely (the manifest is ignored). The editor preview always passes
//! `--replace-network-policy` so a stale `run.toml` beside the binary can never
//! silently inherit network access into an editor-launched preview. A REST
//! datasource only polls when network is enabled **and** its host matches the
//! allowlist; the runtime also rejects non-`http(s)` schemes and endpoints
//! resolving to loopback / link-local / private / cloud-metadata addresses.
//!
//! When launched with **no** `--stage` (a packaged bundle double-clicked, or the
//! cook's runtime binary run in place), the stage and mode fall back to the
//! `run.toml` manifest's `[experience]` section via
//! [`idtx_runtime_core::resolve_run_config`]; an explicit `--stage`/`--mode`
//! always wins.

use std::path::PathBuf;
use std::process::ExitCode;

use idtx_runtime_core::{RunMode, resolve_run_config, run, run_headless};

/// Install a `tracing` subscriber so the runtime's `info!`/`warn!` diagnostics
/// (XR probe, session start, fallbacks) actually reach the console. Without this
/// every `tracing` event is silently dropped. Honours `RUST_LOG`; defaults to
/// `info` so the XR-vs-desktop decision path is visible out of the box.
fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

struct Cli {
    stage: Option<PathBuf>,
    /// `None` when `--mode` was not given, so the manifest's `[experience].mode`
    /// can supply the default without the CLI silently overriding it.
    mode: Option<RunMode>,
    headless: bool,
    frames: Option<u64>,
    /// Bearer token supplied inline via `--token` (leaks into `ps`; prefer
    /// `--token-file`).
    token: Option<String>,
    /// Path supplied via `--token-file`; its trimmed contents are the token.
    token_file: Option<PathBuf>,
    /// Controller device id supplied via `--controller-device` (XR); overrides
    /// the runtime's own `OpenXR` probe when set.
    controller_device: Option<String>,
    /// Absolute directories supplied via (repeatable) `--asset-search-dir`,
    /// installed as the USD default-resolver search path so project-relative
    /// references (`@assets/foo.usdz@`) resolve. The editor preview passes the
    /// open project's asset roots here so a spawned runtime resolves references
    /// exactly as the editor does.
    asset_search_dirs: Vec<PathBuf>,
    /// Fullscreen posture override. `Some(true)` from `--fullscreen`,
    /// `Some(false)` from `--windowed`, `None` when neither was given (so the
    /// manifest's `[experience].fullscreen` supplies the default). Drives
    /// borderless-fullscreen + cursor capture in desktop mode.
    fullscreen: Option<bool>,
    /// `--allow-network`: enable runtime assets, REST and behavior HTTP. ORs
    /// over the manifest's `[network] allow` unless `--replace-network-policy` is
    /// also present, in which case it replaces.
    allow_network: bool,
    /// `--allow-host <pattern>` (repeatable): endpoint host allowlist patterns
    /// appended to the manifest's `[network] allowed_hosts`, or replacing it when
    /// `--replace-network-policy` is present.
    allowed_hosts: Vec<String>,
    /// `--private-host <pattern>`: additional permission for loopback/LAN
    /// addresses, without bypassing `--allow-network` / `--allow-host`.
    private_hosts: Vec<String>,
    /// `--auth-origin <origin>`: explicit credential-sharing scope, independent
    /// of the network allowlist. Replaced together with preview policy.
    auth_origins: Vec<String>,
    /// `--replace-network-policy`: treat `--allow-network` / `--allow-host` as the
    /// *complete* network policy, ignoring the `[network]` section from any
    /// discovered `run.toml`. Without this flag the CLI flags OR-merge over the
    /// manifest. The editor preview always passes this flag so a stale
    /// `run.toml` beside the binary cannot silently inherit network access.
    replace_network_policy: bool,
    /// `--max-response-mb <n>`: cap REST datasource response bodies at `n` MiB.
    /// `None` (flag absent) leaves the manifest's `[network] max_response_mb` in
    /// place; `Some` overrides it (and is the sole value under
    /// `--replace-network-policy`). `0` means "use the runtime default" (1 MiB).
    max_response_mb: Option<u32>,
    /// `--max-asset-mb <n>`: streamed USD asset cap; zero means 256 MiB.
    max_asset_mb: Option<u32>,
    /// `--screenshot <path>`: run headlessly, capture the rendered frame to this
    /// PNG path, then exit. Implies the headless path. Under `openexec` the
    /// capture waits for the compute graph to produce a value first. Needs the
    /// runtime built with the `metrics` feature (the GPU readback is metrics-gated).
    screenshot: Option<PathBuf>,
}

fn parse_cli() -> Result<Cli, String> {
    parse_from(std::env::args().skip(1))
}

/// Parse an already-split argument iterator. Factored out of [`parse_cli`] so
/// the flag grammar (including `--token` / `--token-file`) is unit-testable
/// without touching the process' real argument vector.
fn parse_from(args: impl Iterator<Item = String>) -> Result<Cli, String> {
    let mut stage = None;
    let mut mode = None;
    let mut headless = false;
    let mut frames = None;
    let mut token = None;
    let mut token_file = None;
    let mut controller_device = None;
    let mut asset_search_dirs = Vec::new();
    let mut fullscreen = None;
    let mut allow_network = false;
    let mut allowed_hosts = Vec::new();
    let mut private_hosts = Vec::new();
    let mut auth_origins = Vec::new();
    let mut replace_network_policy = false;
    let mut max_response_mb = None;
    let mut max_asset_mb = None;
    let mut screenshot = None;

    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--stage" => {
                stage = Some(PathBuf::from(args.next().ok_or("--stage requires a path")?));
            }
            "--mode" => {
                mode = Some(RunMode::parse(
                    &args.next().ok_or("--mode requires a value")?,
                ));
            }
            "--headless" => headless = true,
            "--frames" => {
                frames = Some(
                    args.next()
                        .ok_or("--frames requires a count")?
                        .parse::<u64>()
                        .map_err(|e| format!("invalid --frames: {e}"))?,
                );
            }
            "--token" => {
                token = Some(args.next().ok_or("--token requires a value")?);
            }
            "--token-file" => {
                token_file = Some(PathBuf::from(
                    args.next().ok_or("--token-file requires a path")?,
                ));
            }
            "--controller-device" => {
                controller_device =
                    Some(args.next().ok_or("--controller-device requires a value")?);
            }
            "--asset-search-dir" => {
                asset_search_dirs.push(PathBuf::from(
                    args.next().ok_or("--asset-search-dir requires a path")?,
                ));
            }
            "--fullscreen" => fullscreen = Some(true),
            "--windowed" => fullscreen = Some(false),
            "--allow-network" => allow_network = true,
            "--allow-host" => {
                allowed_hosts.push(args.next().ok_or("--allow-host requires a pattern")?);
            }
            "--private-host" => {
                private_hosts.push(args.next().ok_or("--private-host requires a pattern")?);
            }
            "--auth-origin" => {
                let origin = args.next().ok_or("--auth-origin requires an origin")?;
                idtx_auth::AuthScopes::parse(std::slice::from_ref(&origin)).map_err(|e| e.to_string())?;
                auth_origins.push(origin);
            }
            "--replace-network-policy" => replace_network_policy = true,
            "--max-response-mb" => {
                max_response_mb = Some(
                    args.next()
                        .ok_or("--max-response-mb requires a value")?
                        .parse::<u32>()
                        .map_err(|e| format!("invalid --max-response-mb: {e}"))?,
                );
            }
            "--max-asset-mb" => {
                max_asset_mb = Some(args.next().ok_or("--max-asset-mb requires a value")?
                    .parse::<u32>().map_err(|e| format!("invalid --max-asset-mb: {e}"))?);
            }
            "--screenshot" => {
                screenshot = Some(PathBuf::from(
                    args.next().ok_or("--screenshot requires a path")?,
                ));
            }
            other if !other.starts_with("--") && stage.is_none() => {
                stage = Some(PathBuf::from(other));
            }
            // Unknown flags are ignored for forward-compatibility.
            _ => {}
        }
    }

    Ok(Cli {
        stage,
        mode,
        headless,
        frames,
        token,
        token_file,
        controller_device,
        asset_search_dirs,
        fullscreen,
        allow_network,
        allowed_hosts,
        private_hosts,
        auth_origins,
        replace_network_policy,
        max_response_mb,
        max_asset_mb,
        screenshot,
    })
}

fn apply_network_overrides(cfg: &mut idtx_runtime_core::RunConfig, cli: &Cli) {
    if cli.replace_network_policy {
        // A preview cannot relabel a stale bundle's embedded credential with
        // the current project's newly supplied sharing group.
        cfg.dev_bearer_token = None;
        cfg.exec_allow_network = cli.allow_network;
        cfg.exec_allowed_hosts.clone_from(&cli.allowed_hosts);
        cfg.private_hosts.clone_from(&cli.private_hosts);
        cfg.auth_origins.clone_from(&cli.auth_origins);
        cfg.exec_max_response_mb = cli.max_response_mb.unwrap_or(0);
        cfg.max_asset_mb = cli.max_asset_mb.unwrap_or(0);
    } else {
        cfg.exec_allow_network |= cli.allow_network;
        cfg.exec_allowed_hosts.extend(cli.allowed_hosts.iter().cloned());
        cfg.private_hosts.extend(cli.private_hosts.iter().cloned());
        cfg.auth_origins.extend(cli.auth_origins.iter().cloned());
        if let Some(mb) = cli.max_response_mb { cfg.exec_max_response_mb = mb; }
        if let Some(mb) = cli.max_asset_mb { cfg.max_asset_mb = mb; }
    }
}

mod build_info;

fn main() -> ExitCode {
    build_info::retain();
    init_tracing();

    let cli = match parse_cli() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("idtx-runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Layer the CLI over a discovered `run.toml`: an explicit `--stage`/`--mode`
    // wins, otherwise the packaged experience defaults fill in. This also warns
    // if the bundle's behavior ABI pin does not match this runtime.
    let mut cfg = match resolve_run_config(cli.stage.clone(), cli.mode, cli.fullscreen) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("idtx-runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    tracing::info!(
        stage = %cfg.stage.display(),
        mode = ?cfg.mode,
        headless = cli.headless,
        fullscreen = cfg.fullscreen,
        pcvr = cfg!(feature = "pcvr"),
        "idtx-runtime starting"
    );

    apply_network_overrides(&mut cfg, &cli);
    cfg.bearer_token = cli.token;
    cfg.token_file = cli.token_file;
    cfg.controller_device = cli.controller_device;
    // CLI-supplied search dirs (the editor preview's open-project roots) take
    // precedence over the manifest-derived roots seeded in `resolve_run_config`.
    if !cli.asset_search_dirs.is_empty() {
        let mut dirs = cli.asset_search_dirs;
        dirs.append(&mut cfg.asset_search_dirs);
        cfg.asset_search_dirs = dirs;
    }
    // A screenshot capture is inherently a headless run.
    cfg.screenshot = cli.screenshot;
    if cli.headless || cli.frames.is_some() || cfg.screenshot.is_some() {
        cfg.frames = Some(cli.frames.unwrap_or(1));
        return match run_headless(&cfg) {
            Ok(report) => {
                println!(
                    "idtx-runtime: headless run ok — {} frames, {} behaviors, xr_started={}, xr_pose_frames={}, exec_values={}",
                    report.frames,
                    report.attached_behaviors,
                    report.xr_started,
                    report.xr_pose_frames,
                    report.exec_authored
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("idtx-runtime: {e}");
                ExitCode::FAILURE
            }
        };
    }

    match run(cfg) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("idtx-runtime: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> impl Iterator<Item = String> {
        items
            .iter()
            .map(|s| (*s).to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn parses_token_flag() {
        let cli = parse_from(args(&["--stage", "s.usda", "--token", "abc123"])).unwrap();
        assert_eq!(cli.token.as_deref(), Some("abc123"));
        assert!(cli.token_file.is_none());
    }

    #[test]
    fn parses_token_file_flag() {
        let cli = parse_from(args(&["s.usda", "--token-file", "/secrets/t.txt"])).unwrap();
        assert_eq!(cli.token_file, Some(PathBuf::from("/secrets/t.txt")));
        assert!(cli.token.is_none());
    }

    #[test]
    fn token_flag_requires_value() {
        assert!(parse_from(args(&["s.usda", "--token"])).is_err());
        assert!(parse_from(args(&["s.usda", "--token-file"])).is_err());
    }

    #[test]
    fn auth_flags_default_to_none() {
        let cli = parse_from(args(&["s.usda"])).unwrap();
        assert!(cli.token.is_none());
        assert!(cli.token_file.is_none());
        assert!(cli.controller_device.is_none());
    }

    #[test]
    fn parses_controller_device_flag() {
        let cli = parse_from(args(&[
            "--stage",
            "s.usda",
            "--controller-device",
            "questpro",
        ]))
        .unwrap();
        assert_eq!(cli.controller_device.as_deref(), Some("questpro"));
    }

    #[test]
    fn controller_device_flag_requires_value() {
        assert!(parse_from(args(&["s.usda", "--controller-device"])).is_err());
    }

    #[test]
    fn parses_repeatable_asset_search_dir_flag() {
        let cli = parse_from(args(&[
            "--stage",
            "s.usda",
            "--asset-search-dir",
            "/proj",
            "--asset-search-dir",
            "/proj/assets",
        ]))
        .unwrap();
        assert_eq!(
            cli.asset_search_dirs,
            vec![PathBuf::from("/proj"), PathBuf::from("/proj/assets")]
        );
    }

    #[test]
    fn asset_search_dir_flag_requires_value() {
        assert!(parse_from(args(&["s.usda", "--asset-search-dir"])).is_err());
    }

    #[test]
    fn asset_search_dir_defaults_empty() {
        assert!(
            parse_from(args(&["s.usda"]))
                .unwrap()
                .asset_search_dirs
                .is_empty()
        );
    }

    #[test]
    fn mode_is_none_unless_given_and_parses_when_present() {
        // No --mode → None, so the manifest's [experience].mode can win.
        assert!(parse_from(args(&["s.usda"])).unwrap().mode.is_none());
        // --mode desktop → Some(Desktop).
        let cli = parse_from(args(&["s.usda", "--mode", "desktop"])).unwrap();
        assert_eq!(cli.mode, Some(RunMode::Desktop));
    }

    #[test]
    fn unknown_flags_still_ignored_alongside_auth() {
        let cli = parse_from(args(&["--wat", "s.usda", "--token", "x"])).unwrap();
        assert_eq!(cli.stage, Some(PathBuf::from("s.usda")));
        assert_eq!(cli.token.as_deref(), Some("x"));
    }

    #[test]
    fn fullscreen_flags_are_none_unless_given() {
        // Neither flag → None, so the manifest's [experience].fullscreen can win.
        assert!(parse_from(args(&["s.usda"])).unwrap().fullscreen.is_none());
        assert_eq!(
            parse_from(args(&["s.usda", "--fullscreen"]))
                .unwrap()
                .fullscreen,
            Some(true)
        );
        assert_eq!(
            parse_from(args(&["s.usda", "--windowed"]))
                .unwrap()
                .fullscreen,
            Some(false)
        );
    }

    #[test]
    fn network_flags_default_off_and_empty() {
        let cli = parse_from(args(&["s.usda"])).unwrap();
        assert!(!cli.allow_network);
        assert!(cli.allowed_hosts.is_empty());
    }

    #[test]
    fn parses_allow_network_and_repeatable_allow_host() {
        let cli = parse_from(args(&[
            "--stage",
            "s.usda",
            "--allow-network",
            "--allow-host",
            "api.example.com",
            "--allow-host",
            "*.data.example.org",
        ]))
        .unwrap();
        assert!(cli.allow_network);
        assert_eq!(
            cli.allowed_hosts,
            vec![
                "api.example.com".to_string(),
                "*.data.example.org".to_string()
            ]
        );
    }

    #[test]
    fn allow_host_requires_value() {
        assert!(parse_from(args(&["s.usda", "--allow-host"])).is_err());
    }

    #[test]
    fn auth_origins_are_explicit_and_independent_of_allowed_hosts() {
        let cli = parse_from(args(&["s.usda", "--allow-host", "unrelated.example",
            "--auth-origin", "https://*.assets.example:8443"])).unwrap();
        assert_eq!(cli.auth_origins, ["https://*.assets.example:8443"]);
        assert_eq!(cli.allowed_hosts, ["unrelated.example"]);
        assert!(parse_from(args(&["s.usda", "--auth-origin", "https://host/path"])).is_err());
        assert!(parse_from(args(&["s.usda", "--auth-origin"])).is_err());
    }

    #[test]
    fn preview_policy_replacement_discards_stale_embedded_credentials() {
        let cli = parse_from(args(&["s.usda", "--replace-network-policy", "--auth-origin", "https://new.example"])).unwrap();
        let mut cfg = idtx_runtime_core::RunConfig::new("s.usda".into(), RunMode::Desktop);
        cfg.auth_origins = vec!["https://old.example".into()];
        cfg.dev_bearer_token = Some("old-secret".into());
        cfg.private_hosts = vec!["old.internal".into()];
        cfg.max_asset_mb = 64;
        apply_network_overrides(&mut cfg, &cli);
        assert_eq!(cfg.auth_origins, ["https://new.example"]);
        assert!(cfg.dev_bearer_token.is_none());
        assert!(cfg.private_hosts.is_empty());
        assert_eq!(cfg.max_asset_mb, 0);
        assert!(!cfg.exec_allow_network);
    }

    #[test]
    fn private_network_and_asset_cap_cli_overrides_are_independent() {
        let cli = parse_from(args(&["s.usda", "--private-host", "assets.internal", "--max-asset-mb", "64"])).unwrap();
        let mut cfg = idtx_runtime_core::RunConfig::new("s.usda".into(), RunMode::Desktop);
        apply_network_overrides(&mut cfg, &cli);
        assert_eq!(cfg.private_hosts, ["assets.internal"]);
        assert_eq!(cfg.max_asset_mb, 64);
        assert!(!cfg.exec_allow_network);
        assert!(cfg.exec_allowed_hosts.is_empty());
        for invalid in [&["--private-host"][..], &["--max-asset-mb"], &["--max-asset-mb", "-1"]] {
            assert!(parse_from(args(invalid)).is_err());
        }
    }

    #[test]
    fn max_response_mb_defaults_none_and_parses() {
        assert_eq!(
            parse_from(args(&["s.usda"])).unwrap().max_response_mb,
            None
        );
        assert_eq!(
            parse_from(args(&["s.usda", "--max-response-mb", "8"]))
                .unwrap()
                .max_response_mb,
            Some(8)
        );
    }

    #[test]
    fn max_response_mb_requires_valid_value() {
        assert!(parse_from(args(&["s.usda", "--max-response-mb"])).is_err());
        assert!(parse_from(args(&["s.usda", "--max-response-mb", "huge"])).is_err());
    }

    #[test]
    fn replace_network_policy_defaults_false() {
        let cli = parse_from(args(&["s.usda"])).unwrap();
        assert!(!cli.replace_network_policy);
    }

    #[test]
    fn replace_network_policy_flag_parsed() {
        let cli =
            parse_from(args(&["s.usda", "--replace-network-policy"])).unwrap();
        assert!(cli.replace_network_policy);
        assert!(!cli.allow_network);
        assert!(cli.allowed_hosts.is_empty());
    }

    #[test]
    fn replace_network_policy_with_allow_and_hosts() {
        let cli = parse_from(args(&[
            "s.usda",
            "--replace-network-policy",
            "--allow-network",
            "--allow-host",
            "api.example.com",
        ]))
        .unwrap();
        assert!(cli.replace_network_policy);
        assert!(cli.allow_network);
        assert_eq!(cli.allowed_hosts, vec!["api.example.com".to_string()]);
    }
}
