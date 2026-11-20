//! `cargo xtask-visionos bootstrap-visionos-signing` — one-time credential
//! bootstrap for free Personal-Team on-device deployment.
//!
//! A physical Vision Pro needs a real **Apple Development** certificate + a
//! **development provisioning profile** that lists the device UDID and matches the
//! app's bundle id. On a free team those are minted only by Xcode's automatic
//! signing — there is no web portal. This command removes the manual-Xcode-project
//! step: it generates a throwaway visionOS app for the target bundle id and runs
//!
//! ```text
//! xcodebuild -allowProvisioningUpdates -allowProvisioningDeviceRegistration \
//!   -destination 'platform=visionOS,id=<UDID>' build
//! ```
//!
//! which (with a free Apple ID already added in `Xcode > Settings > Accounts`)
//! mints the cert, registers the device, and creates the profile. Afterwards
//! `run-visionos-device` auto-discovers both.
//!
//! The project-generation + `xcodebuild` + team-detection primitives are shared
//! with the Conductor build thread, so they live in `idtx-build`
//! ([`idtx_build::bootstrap_signing`] & friends); this module is the thin CLI
//! wrapper that resolves the device/team, prints the plan, and — because a free
//! team usually still needs a one-time Xcode GUI mint — opens the generated
//! project in Xcode when the headless `xcodebuild` fails.
//!
//! The one thing it cannot do is add the Apple ID to Xcode — that requires
//! interactive Apple authentication, so it stays a manual step.

// Apple product names (Xcode, xcodebuild, Apple ID, visionOS) read fine
// unbackticked throughout this module's docs.
#![allow(clippy::doc_markdown)]

use anyhow::{Context, Result, bail};

use idtx_build::{
    SigningBootstrapPlan, VisionOsBootstrapError, bootstrap_signing, bootstrap_xcodebuild_args,
    detect_free_team, list_paired_devices, looks_like_team_id, select_visionos_device,
};

use crate::visionos_device::DEFAULT_BUNDLE_ID;
use crate::visionos_runtime::workspace_root;

/// Parsed `bootstrap-visionos-signing` options.
#[derive(Debug, Clone, PartialEq, Eq, Default, clap::Args)]
pub struct BootstrapOptions {
    /// Bundle id to provision (default [`DEFAULT_BUNDLE_ID`] — what
    /// `run-visionos-device` signs).
    #[arg(long = "bundle-id", value_name = "ID")]
    pub bundle_id: Option<String>,
    /// Signing team id; auto-detected from Xcode's registered Apple IDs when unset.
    #[arg(long, value_name = "TEAMID")]
    pub team: Option<String>,
    /// Target device UDID; the paired Vision Pro is auto-selected when unset.
    #[arg(long, value_name = "UDID")]
    pub device: Option<String>,
    /// Generate the project + print the `xcodebuild` plan, but do not run it.
    #[arg(long)]
    pub dry_run: bool,
}


/// `cargo xtask-visionos bootstrap-visionos-signing [--bundle-id <id>] [--team
/// <TEAMID>] [--device <UDID>] [--dry-run]`. macOS only.
pub fn bootstrap_visionos_signing_cmd(opts: &BootstrapOptions) -> Result<()> {
    if !cfg!(target_os = "macos") && !opts.dry_run {
        bail!("bootstrap-visionos-signing must run on macOS (needs xcodebuild). Use --dry-run elsewhere.");
    }
    let bundle_id = opts.bundle_id.clone().unwrap_or_else(|| DEFAULT_BUNDLE_ID.to_owned());

    // Resolve the signing Team ID (10 uppercase-alphanumeric chars, e.g.
    // "SHD387D78J" — NOT the Apple ID email). An explicit valid --team wins;
    // otherwise auto-detect the free Personal Team from Xcode's account store.
    let team = resolve_team(opts.team.clone());
    if team.is_none() && !opts.dry_run {
        bail!(
            "could not determine a signing Team ID. Add your Apple ID in Xcode > Settings > \
             Apple Accounts (it becomes a free \"Personal Team\"), then re-run — or pass \
             --team <TEAMID> (10 chars, e.g. SHD387D78J)."
        );
    }

    // Resolve the target device UDID (auto-select the paired Vision Pro).
    let device = match opts.device.clone() {
        Some(d) => d,
        None if opts.dry_run => "<paired-vision-pro-UDID>".to_owned(),
        None => {
            let devices = list_paired_devices().context("listing paired devices via devicectl")?;
            select_visionos_device(&devices)
                .map(|d| d.identifier.clone())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no paired Apple Vision Pro found. Pair it in Xcode (Window > Devices and \
                         Simulators) with Developer Mode on, or pass --device <UDID>."
                    )
                })?
        }
    };

    let proj_dir = workspace_root()
        .join("target")
        .join("visionos-signing-bootstrap");
    let xcodeproj = proj_dir.join(format!("{}.xcodeproj", idtx_build::BOOTSTRAP_APP));

    println!("bootstrap-visionos-signing plan:");
    println!("  Bundle id: {bundle_id}");
    println!("  Team:      {}", team.as_deref().unwrap_or("<auto-select account>"));
    println!("  Device:    {device}");
    println!("  Project:   {}", xcodeproj.display());
    let xcodebuild_args =
        bootstrap_xcodebuild_args(&xcodeproj, team.as_deref(), &bundle_id, &device);
    println!("  $ xcodebuild {}", xcodebuild_args.join(" "));

    if opts.dry_run {
        println!("bootstrap-visionos-signing: --dry-run, stopping before generating/running.");
        return Ok(());
    }

    // Generate + run xcodebuild through the shared core. `cleanup: false` leaves
    // the project on disk on failure so we can open it in Xcode for the one-time
    // free-team GUI mint.
    match bootstrap_signing(&SigningBootstrapPlan {
        bundle_id: &bundle_id,
        team: team.as_deref(),
        device_udid: &device,
        scratch_dir: &proj_dir,
        cleanup: false,
    }) {
        Ok(()) => {
            println!(
                "\n\u{2705}  Signing bootstrapped for bundle id `{bundle_id}` on device {device}.\n\
                 The Apple Development cert is in your keychain and a development provisioning profile\n\
                 was created (expires in ~7 days). Now deploy the runtime:\n    \
                 cargo xtask-visionos run-visionos-device --stage <usda>\n\
                 (Re-run this bootstrap, or just re-run run-visionos-device, after the ~7-day expiry.)"
            );
            Ok(())
        }
        Err(VisionOsBootstrapError::XcodebuildFailed { xcodeproj, .. }) => {
            // Free Personal Teams usually CANNOT be provisioned headlessly by
            // xcodebuild ("No Accounts" / "missing Xcode-Username"): Apple only drives
            // the free-team automatic-signing service from the Xcode GUI, not the CLI.
            // Fall back to opening the generated project so one GUI Run mints
            // everything (cert + profile + device registration).
            let xcodeproj = xcodeproj.unwrap_or_else(|| {
                proj_dir.join(format!("{}.xcodeproj", idtx_build::BOOTSTRAP_APP))
            });
            eprintln!(
                "\n\u{26A0}\u{FE0F}  xcodebuild could not mint credentials from the CLI.\n\
                 For a FREE Personal Team this is expected — Apple only mints free-team cert +\n\
                 profiles through the Xcode GUI (the CLI reports \"No Accounts\"). Opening the\n\
                 throwaway project in Xcode; do this ONCE:\n  \
                   1. Top bar: pick your Vision Pro ({device}) as the run destination.\n  \
                   2. Target > Signing & Capabilities: Team = your Personal Team,\n     \
                      \u{201C}Automatically manage signing\u{201D} on.\n  \
                   3. Press Run (\u{2318}R). Xcode mints the Apple Development cert + a development\n     \
                      provisioning profile for `{bundle_id}` and registers the headset.\n  \
                   4. On the headset, trust the developer: Settings > General > VPN & Device Management.\n\
                 Then deploy the real runtime headlessly:\n    \
                   cargo xtask-visionos run-visionos-device --stage <usda>"
            );
            if let Err(e) = std::process::Command::new("open").arg(&xcodeproj).status() {
                eprintln!(
                    "  (could not auto-open Xcode: {e}; open {} yourself)",
                    xcodeproj.display()
                );
            }
            bail!("credential minting must be completed once in Xcode (steps above); then re-run run-visionos-device.");
        }
        Err(e) => Err(e).context(
            "spawning xcodebuild (is Xcode's command-line toolchain selected? `xcode-select -p`)",
        ),
    }
}

/// Resolve the signing Team ID: an explicit valid `--team` wins; otherwise
/// auto-detect the free Personal Team from Xcode's account store.
fn resolve_team(explicit: Option<String>) -> Option<String> {
    match explicit {
        Some(t) if looks_like_team_id(&t) => Some(t),
        Some(t) => {
            eprintln!(
                "  note: --team '{t}' is not a 10-char Team ID (looks like an Apple ID?); \
                 auto-detecting your Personal Team instead."
            );
            detect_free_team()
        }
        None => detect_free_team(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse_opts(args: &[&str]) -> Result<BootstrapOptions, clap::Error> {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            opts: BootstrapOptions,
        }
        TestCli::try_parse_from(
            std::iter::once("bootstrap-visionos-signing").chain(args.iter().copied()),
        )
        .map(|c| c.opts)
    }

    #[test]
    fn options_parse() {
        let o = parse_opts(&[
            "--bundle-id",
            "com.example.x",
            "--team",
            "TEAMID1234",
            "--device",
            "UDID-9",
        ])
        .unwrap();
        assert_eq!(o.bundle_id.as_deref(), Some("com.example.x"));
        assert_eq!(o.team.as_deref(), Some("TEAMID1234"));
        assert_eq!(o.device.as_deref(), Some("UDID-9"));
        assert!(parse_opts(&["--bogus"]).is_err());
    }

    #[test]
    fn generate_project_is_reachable() {
        // The project-generation primitive now lives in idtx-build; smoke-test the
        // re-exported path (the detailed assertions live in that crate's tests).
        let tmp =
            std::env::temp_dir().join(format!("idtx-xtask-bootstrap-{}", std::process::id()));
        let xcodeproj =
            idtx_build::generate_bootstrap_project(&tmp, DEFAULT_BUNDLE_ID).expect("generate");
        assert!(xcodeproj.join("project.pbxproj").is_file());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
