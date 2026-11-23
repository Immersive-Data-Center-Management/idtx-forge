//! `cargo xtask run-visionos-device` — build the **device-slice** `idtx-runtime`,
//! assemble a **signed, launchable** visionOS `.app`, and install + launch it on a
//! **physical Apple Vision Pro** via `devicectl`.
//!
//! This is the free **Personal-Team** on-device path: no paid Apple Developer
//! Program, no App Store, no notarization. The one-time `Apple Development`
//! certificate + development provisioning profile are minted by Xcode /
//! `xcodebuild -allowProvisioningUpdates` (a free team has no web portal); this
//! command *consumes* them — auto-discovered from the login keychain +
//! `~/Library/MobileDevice/Provisioning Profiles`, or pinned via `--identity` /
//! `--profile`.
//!
//! # Pipeline
//!
//! 1. Cross-build the device slice via [`crate::visionos_runtime::build_slice`]
//!    (`aarch64-apple-visionos`, `xros` SDK) WITH the `visionos-immersive` feature.
//! 2. Stage the flat `.app`: the runtime dylib + USD plugInfo tree in `Frameworks/`,
//!    the bundled stage, and the device `Info.plist`.
//! 3. [`idtx_build::finish_device_app`] embeds the full dylib closure, fixes
//!    install-names/rpaths, compiles the Swift `CompositorServices` shell, embeds the
//!    provisioning profile, and signs for the device (free-team sequence: no
//!    hardened runtime, no notarization).
//! 4. Unless `--no-install`, `devicectl` installs + launches on the paired headset.
//!
//! # Testability (TMDD)
//!
//! The assembler, signer, and `devicectl` arg builders live in `idtx-build` and are
//! unit-tested there; this module is thin orchestration. `--dry-run` prints the
//! plan + the `devicectl` arg vectors on any host.

// Apple product/identifier names (ARKit, Info.plist, Xcode, CompositorServices,
// devicectl) read fine unbackticked throughout this module's docs.
#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use idtx_build::{
    DeviceAppPlan, VisionOsPlistOptions, copy_from, copy_from_args, discover_development_identity,
    discover_profile_for, finish_device_app, install_app, install_app_args, launch_app,
    launch_args, list_paired_devices, read_profile, select_visionos_device,
    visionos_info_plist_with,
};

use crate::usd_cross::VisionOsSlice;
use crate::visionos_runtime::{build_slice, clang_target, copy_into, copy_tree_recursive, workspace_root};

/// The device app's bundle name / executable stem.
pub const DEVICE_APP_NAME: &str = "IdtxVision";
/// The default device bundle id (reconciled against the provisioning profile).
pub const DEFAULT_BUNDLE_ID: &str = "com.sap.apeirora.idtx.vision";
/// The resource name the bundled stage is copied to (`<name>.<ext>`).
pub const STAGE_RESOURCE: &str = "stage";
/// Default stage bundled when `--stage` is not given.
pub const DEFAULT_STAGE: &str = "tests/fixtures/usd/hydration/authored_visibility.usda";

/// Parsed `run-visionos-device` options.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[allow(clippy::struct_excessive_bools)] // CLI flag bag; each flag is independent.
pub struct RunDeviceOptions {
    /// Build the `release` profile. Overrides an earlier `--debug`. Default
    /// `false` (debug).
    #[arg(long, overrides_with = "debug")]
    pub release: bool,
    /// Build the `debug` profile (the default); overrides an earlier
    /// `--release` (last profile flag wins).
    #[arg(long, overrides_with = "release")]
    pub debug: bool,
    /// The stage to bundle + open (`--stage <path>`). Default [`DEFAULT_STAGE`].
    #[arg(long, default_value = DEFAULT_STAGE, value_name = "PATH")]
    pub stage: PathBuf,
    /// Target device UDID/identifier (`--device`); auto-selected when unset.
    #[arg(long, value_name = "UDID")]
    pub device: Option<String>,
    /// Signing identity override (`--identity`); auto-discovered when unset.
    #[arg(long, value_name = "ID")]
    pub identity: Option<String>,
    /// Provisioning-profile override (`--profile`); auto-discovered when unset.
    #[arg(long, value_name = "PATH")]
    pub profile: Option<PathBuf>,
    /// Bundle id override (`--bundle-id`); default [`DEFAULT_BUNDLE_ID`].
    #[arg(long = "bundle-id", value_name = "ID")]
    pub bundle_id: Option<String>,
    /// Enable ARKit hand tracking (adds the `Info.plist` usage string).
    #[arg(long = "hand-tracking")]
    pub hand_tracking: bool,
    /// Enable the `metrics` feature so the immersive runtime records per-eye
    /// render timings and dumps `idtx-xr-metrics-<stage>-<ts>.json` into the
    /// app's `tmp/` container (pull with `cargo xtask pull-visionos-metrics`).
    /// `--with-metrics`.
    #[arg(long = "with-metrics", alias = "with_metrics")]
    pub with_metrics: bool,
    /// Assemble + sign but skip the `devicectl` install/launch.
    #[arg(long = "no-install")]
    pub no_install: bool,
    /// Resolve + print the plan, but do not shell out.
    #[arg(long)]
    pub dry_run: bool,
}

impl Default for RunDeviceOptions {
    fn default() -> Self {
        Self {
            release: false,
            debug: false,
            stage: PathBuf::from(DEFAULT_STAGE),
            device: None,
            identity: None,
            profile: None,
            bundle_id: None,
            hand_tracking: false,
            with_metrics: false,
            no_install: false,
            dry_run: false,
        }
    }
}

impl RunDeviceOptions {
    fn profile_name(&self) -> &'static str {
        // `debug`/`release` are mutually exclusive via overrides_with (last flag
        // wins); either explicitly set resolves the profile, defaulting to debug.
        if self.release && !self.debug { "release" } else { "debug" }
    }
}

/// `cargo xtask run-visionos-device [...]`. macOS only (needs the visionOS SDK +
/// `swiftc` + `codesign` + `devicectl`); `--dry-run` prints the plan on any host.
pub fn run_visionos_device_cmd(opts: &RunDeviceOptions) -> Result<()> {
    if !cfg!(target_os = "macos") && !opts.dry_run {
        bail!(
            "run-visionos-device must run on macOS (needs xcrun + swiftc + codesign + devicectl). \
             Use --dry-run to inspect the plan on another host."
        );
    }

    let root = workspace_root();
    let out_root = root
        .join("target")
        .join(opts.profile_name())
        .join("visionos-device");
    let app = out_root.join(format!("{DEVICE_APP_NAME}.app"));
    let stage_abs = if opts.stage.is_absolute() {
        opts.stage.clone()
    } else {
        root.join(&opts.stage)
    };
    let configured_bundle_id = opts
        .bundle_id
        .clone()
        .unwrap_or_else(|| DEFAULT_BUNDLE_ID.to_owned());

    println!("run-visionos-device plan:");
    println!("  Slice:     aarch64-apple-visionos (xros)");
    println!("  Profile:   {}", opts.profile_name());
    println!("  Stage:     {}", stage_abs.display());
    println!("  App:       {}", app.display());
    println!("  Bundle id: {configured_bundle_id} (reconciled against the profile)");
    println!(
        "  Identity:  {}",
        opts.identity.as_deref().unwrap_or("<auto-discover Apple Development>")
    );
    println!(
        "  Profile:   {}",
        opts.profile
            .as_deref()
            .map_or_else(|| "<auto-discover newest matching>".to_owned(), |p| p.display().to_string())
    );
    println!("  Install:   {}", if opts.no_install { "no (assemble + sign only)" } else { "devicectl install + launch" });

    if opts.dry_run {
        let udid = opts.device.as_deref().unwrap_or("<paired-vision-pro-UDID>");
        println!("  devicectl: xcrun {}", install_app_args(udid, &app).join(" "));
        println!("  devicectl: xcrun {}", launch_args(udid, &configured_bundle_id).join(" "));
        println!("run-visionos-device: --dry-run, stopping before any process is spawned.");
        return Ok(());
    }

    if !stage_abs.is_file() {
        bail!("stage to bundle not found: {}", stage_abs.display());
    }

    // 1) Resolve device signing credentials up front (the bundle id must match the
    //    profile before the Info.plist is written).
    let identity = match opts.identity.clone() {
        Some(id) => id,
        None => discover_development_identity()
            .context("resolving an Apple Development signing identity")?,
    };
    let profile_path = match opts.profile.clone() {
        Some(p) => p,
        None => discover_profile_for(&configured_bundle_id)
            .context("resolving a development provisioning profile")?,
    };
    let provision =
        read_profile(&profile_path).context("reading the provisioning profile")?;
    let bundle_id = provision.resolved_bundle_id(&configured_bundle_id);
    let team_id = provision.team_id.clone();
    println!("  Resolved:  identity={identity}");
    println!("             team={team_id} bundle={bundle_id}");
    println!("             profile={}", profile_path.display());
    if !provision.provisioned_devices.is_empty() {
        println!("             profile provisions {} device(s)", provision.provisioned_devices.len());
    }

    // 2) Cross-build the device slice WITH the immersive renderer.
    let mut features = vec!["visionos-immersive", "gesture-recognition"];
    if opts.with_metrics {
        features.push("metrics");
    }
    let built = build_slice(
        VisionOsSlice::Device,
        opts.release,
        None,
        &features,
    )?;

    // 3) Stage the flat `.app`: runtime dylib + USD plugInfo + bundled stage + plist.
    let stage_ext = stage_abs
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("usda")
        .to_owned();
    stage_device_app(&app, &built.produced_dylib, &built.prefixes.usd_dir, &stage_abs, &stage_ext)?;
    let exe = app.join(DEVICE_APP_NAME);
    std::fs::write(
        app.join("Info.plist"),
        visionos_info_plist_with(&VisionOsPlistOptions {
            name: DEVICE_APP_NAME,
            executable: DEVICE_APP_NAME,
            simulator: false,
            bundle_id: Some(&bundle_id),
            hand_tracking: opts.hand_tracking,
        }),
    )
    .with_context(|| format!("writing {}", app.join("Info.plist").display()))?;

    // 4) Finish: embed the dylib closure, fix rpaths, compile the Swift shell,
    //    embed the profile, and sign for the device.
    let frameworks = app.join("Frameworks");
    let lib_dirs = [
        built.prefixes.usd_dir.join("lib"),
        built.prefixes.tbb_prefix.join("lib"),
        built.prefixes.materialx_prefix.join("lib"),
    ];
    finish_device_app(&DeviceAppPlan {
        app: &app,
        frameworks: &frameworks,
        runtime_dylib_name: "libidtx_runtime.dylib",
        lib_dirs: &lib_dirs,
        stage_resource: STAGE_RESOURCE,
        stage_ext: &stage_ext,
        sdk: "xros",
        clang_target: &clang_target(VisionOsSlice::Device, &built.deployment_target),
        exe: &exe,
        identity: &identity,
        provisioning_profile: &profile_path,
        team_id: &team_id,
        bundle_id: &bundle_id,
        hand_tracking: opts.hand_tracking,
    })
    .context("assembling + signing the visionOS device .app")?;
    println!("  signed {}", app.display());

    // 5) Install + launch on the paired Vision Pro.
    if opts.no_install {
        println!("\n\u{2705}  Signed {} ({bundle_id}). --no-install: skipping deploy.", app.display());
        println!("  Install manually: xcrun {}", install_app_args("<UDID>", &app).join(" "));
        return Ok(());
    }
    deploy(opts.device.as_deref(), &app, &bundle_id)?;

    println!(
        "\n\u{2705}  {DEVICE_APP_NAME} is installed on the Vision Pro.\n\
         Launch it from the headset's Home View (immersive/full-space apps often can't be\n\
         launched headlessly via devicectl). Free-team note: the development provisioning\n\
         profile expires after ~7 days — re-run this command to re-sign + re-deploy."
    );
    Ok(())
}

/// Stage the flat `.app` skeleton: clean, create `Frameworks/`, copy the runtime
/// dylib + USD plugInfo tree, and bundle the stage. (The dylib closure, rpath
/// fixups, Swift shell, and signing are done by [`finish_device_app`].)
fn stage_device_app(
    app: &Path,
    runtime_dylib: &Path,
    usd_dir: &Path,
    stage: &Path,
    stage_ext: &str,
) -> Result<()> {
    if app.exists() {
        std::fs::remove_dir_all(app)
            .with_context(|| format!("clearing old bundle {}", app.display()))?;
    }
    let frameworks = app.join("Frameworks");
    std::fs::create_dir_all(&frameworks)
        .with_context(|| format!("creating {}", frameworks.display()))?;

    // The runtime cdylib (install-name/rpath fixups applied by finish_device_app).
    copy_into(runtime_dylib, &frameworks)?;

    // OpenUSD plugInfo discovery tree beside the dylibs.
    let usd_tree = usd_dir.join("lib").join("usd");
    if usd_tree.is_dir() {
        copy_tree_recursive(&usd_tree, &frameworks.join("usd"))
            .with_context(|| format!("staging USD plugInfo tree {}", usd_tree.display()))?;
    } else {
        eprintln!(
            "  warning: USD plugInfo tree {} absent; opening a stage will fail",
            usd_tree.display()
        );
    }

    // The bundled stage (named `<STAGE_RESOURCE>.<ext>` for Bundle.main lookup).
    let bundled = app.join(format!("{STAGE_RESOURCE}.{stage_ext}"));
    std::fs::copy(stage, &bundled)
        .with_context(|| format!("bundling stage {}", stage.display()))?;
    Ok(())
}

/// Select the paired Vision Pro (or honor `--device`) and `devicectl` install +
/// launch the signed `.app`.
fn deploy(device: Option<&str>, app: &Path, bundle_id: &str) -> Result<()> {
    let udid = if let Some(d) = device {
        d.to_owned()
    } else {
        let devices = list_paired_devices().context("listing paired devices via devicectl")?;
        let vp = select_visionos_device(&devices).ok_or_else(|| {
            anyhow::anyhow!(
                "no paired Apple Vision Pro found. Pair it in Xcode (Window > Devices and \
                 Simulators), enable Developer Mode on the headset, and retry (or pass \
                 --device <UDID>)."
            )
        })?;
        println!("  target device: {} ({})", vp.name, vp.identifier);
        vp.identifier.clone()
    };
    install_app(&udid, app).context("devicectl install")?;
    println!("  installed; launching…");
    if let Err(e) = launch_app(&udid, bundle_id) {
        // The very common free-team first-launch case: the app is installed and
        // correctly signed, but the developer/profile has not been trusted on the
        // headset yet, so the launch is denied. That is a one-time manual step —
        // report it as guidance, not a hard failure (the app IS on the device).
        let msg = e.to_string();
        let needs_trust = msg.contains("explicitly trusted")
            || msg.contains("No provider was found")
            || msg.contains("invalid code signature or inadequate entitlements");
        if needs_trust {
            println!(
                "\n\u{2139}\u{FE0F}  Installed, but the first launch needs on-device trust (free-team apps):\n  \
                   On the Vision Pro: Settings > General > VPN & Device Management >\n  \
                   trust the \u{201C}Apple Development\u{201D} developer certificate.\n  \
                 Then launch the \u{201C}IdtxVision\u{201D} app from the Home View, or re-run this command."
            );
            return Ok(());
        }
        return Err(e).context("devicectl launch");
    }
    Ok(())
}

/// Parsed `pull-visionos-metrics` options.
#[derive(Debug, Clone, PartialEq, Eq, Default, clap::Args)]
pub struct PullMetricsOptions {
    /// Target device UDID (`--device`); auto-selected when unset.
    #[arg(long, value_name = "UDID")]
    pub device: Option<String>,
    /// Bundle id override (`--bundle-id`); default [`DEFAULT_BUNDLE_ID`].
    #[arg(long = "bundle-id", value_name = "ID")]
    pub bundle_id: Option<String>,
    /// Local destination directory (`--out`); default `target/visionos-metrics/`.
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,
    /// Container-relative source to copy (`--source`); default `tmp` (the whole
    /// writable temp subtree, which carries both the metrics JSON dumps and the
    /// diagnostic `idtx_visionos.log`).
    #[arg(long, value_name = "REL")]
    pub source: Option<String>,
    /// Resolve + print the plan, but do not shell out.
    #[arg(long)]
    pub dry_run: bool,
}

/// `cargo xtask pull-visionos-metrics [--device <udid>] [--bundle-id <id>]
/// [--out <dir>] [--source <rel>] [--dry-run]`.
///
/// Copies the on-device render-metrics dumps (and, by default, the whole `tmp/`
/// subtree — which also carries `idtx_visionos.log`) out of the immersive app's
/// data container via `devicectl device copy from`. Run after a `--with-metrics`
/// build has been deployed and exercised for a few seconds. macOS only (needs
/// `xcrun` + `devicectl`); `--dry-run` prints the plan on any host.
pub fn pull_visionos_metrics_cmd(opts: &PullMetricsOptions) -> Result<()> {
    let bundle_id = opts
        .bundle_id
        .clone()
        .unwrap_or_else(|| DEFAULT_BUNDLE_ID.to_owned());
    let source = opts.source.clone().unwrap_or_else(|| "tmp".to_owned());
    let out = opts
        .out
        .clone()
        .unwrap_or_else(|| workspace_root().join("target").join("visionos-metrics"));

    println!("pull-visionos-metrics plan:");
    println!("  Bundle:    {bundle_id}");
    println!("  Source:    <appDataContainer>/{source}");
    println!("  Out:       {}", out.display());

    if opts.dry_run {
        let udid = opts.device.as_deref().unwrap_or("<paired-vision-pro-UDID>");
        println!(
            "  devicectl: xcrun {}",
            copy_from_args(udid, &bundle_id, &source, &out).join(" ")
        );
        println!("pull-visionos-metrics: --dry-run, stopping before any process is spawned.");
        return Ok(());
    }

    if !cfg!(target_os = "macos") {
        bail!(
            "pull-visionos-metrics must run on macOS (needs xcrun + devicectl). \
             Use --dry-run to inspect the planned invocation on another host."
        );
    }

    // Resolve the target device: honor --device, else pick the most-reachable
    // paired Vision Pro (same selection as `run-visionos-device`).
    let udid = match opts.device.clone() {
        Some(d) => d,
        None => {
            let devices = list_paired_devices().context("listing paired devices via devicectl")?;
            select_visionos_device(&devices)
                .map(|d| d.identifier.clone())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no reachable paired Vision Pro found (pass --device <UDID>; \
                         list with `xcrun devicectl list devices`)"
                    )
                })?
        }
    };

    std::fs::create_dir_all(&out)
        .with_context(|| format!("creating output dir {}", out.display()))?;
    copy_from(&udid, &bundle_id, &source, &out)
        .context("copying metrics out of the app data container via devicectl")?;

    println!(
        "\n\u{2705}  Pulled <appDataContainer>/{source} from {udid} into {}.\n\
         Look for the newest `idtx-xr-metrics-<stage>-<ts>.json` (and `idtx_visionos.log`) there.",
        out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse_device(args: &[&str]) -> Result<RunDeviceOptions, clap::Error> {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            opts: RunDeviceOptions,
        }
        TestCli::try_parse_from(std::iter::once("run-visionos-device").chain(args.iter().copied()))
            .map(|c| c.opts)
    }

    fn parse_pull(args: &[&str]) -> Result<PullMetricsOptions, clap::Error> {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            opts: PullMetricsOptions,
        }
        TestCli::try_parse_from(
            std::iter::once("pull-visionos-metrics").chain(args.iter().copied()),
        )
        .map(|c| c.opts)
    }

    #[test]
    fn pull_metrics_options_parse() {
        let o = parse_pull(&[
            "--device",
            "UDID-9",
            "--bundle-id",
            "com.example.vp",
            "--out",
            "/tmp/m",
            "--dry-run",
        ])
        .unwrap();
        assert_eq!(o.device.as_deref(), Some("UDID-9"));
        assert_eq!(o.bundle_id.as_deref(), Some("com.example.vp"));
        assert_eq!(o.out, Some(PathBuf::from("/tmp/m")));
        assert!(o.dry_run);
        assert_eq!(PullMetricsOptions::default().source, None);
        assert!(parse_pull(&["--bogus"]).is_err());
    }

    #[test]
    fn options_defaults_and_parse() {
        let o = RunDeviceOptions::default();
        assert!(!o.release && !o.dry_run && !o.no_install);
        assert_eq!(o.stage, PathBuf::from(DEFAULT_STAGE));
        assert!(workspace_root().join(DEFAULT_STAGE).is_file(), "required default stage is missing");
        assert_eq!(o.profile_name(), "debug");

        let o = parse_device(&[
            "--release",
            "--stage",
            "foo.usda",
            "--device",
            "UDID-1",
            "--identity",
            "Apple Development: Jane (TEAM)",
            "--profile",
            "dev.mobileprovision",
            "--bundle-id",
            "com.example.app",
            "--hand-tracking",
            "--no-install",
        ])
        .unwrap();
        assert!(o.release && o.no_install && o.hand_tracking);
        assert_eq!(o.profile_name(), "release");
        assert_eq!(o.device.as_deref(), Some("UDID-1"));
        assert_eq!(o.identity.as_deref(), Some("Apple Development: Jane (TEAM)"));
        assert_eq!(o.profile, Some(PathBuf::from("dev.mobileprovision")));
        assert_eq!(o.bundle_id.as_deref(), Some("com.example.app"));
        assert!(parse_device(&["--bogus"]).is_err());
    }
}
