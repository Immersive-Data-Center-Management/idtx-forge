//! Shared free-team signing bootstrap: generate a throwaway visionOS Xcode
//! project for a given bundle id and run `xcodebuild -allowProvisioningUpdates
//! -allowProvisioningDeviceRegistration` to mint the **Apple Development**
//! certificate + a **development provisioning profile** + register the paired
//! Vision Pro.
//!
//! A physical Vision Pro needs a real cert and a development profile whose
//! `application-identifier` matches the app's bundle id. On a free "Personal
//! Team" those are only minted by Xcode's automatic signing — there is no web
//! portal. This module packages the throwaway-project + `xcodebuild` dance so it
//! can be driven from **both** `xtask` (the `bootstrap-visionos-signing`
//! command) and the Conductor build thread (`idtx-studio-shell`), which both
//! depend on `idtx-build` — the same lifting rationale as
//! [`crate::apple_tools`] / [`crate::apple_identity`].
//!
//! The pure helpers ([`looks_like_team_id`], [`parse_free_team_id`],
//! [`bootstrap_xcodebuild_args`], [`generate_bootstrap_project`]) compile and are
//! unit-tested everywhere; the shell-outs ([`detect_free_team`],
//! [`bootstrap_signing`]) only run on macOS (they need `xcodebuild` / `defaults`)
//! and are gated `#[cfg(target_os = "macos")]`.

// Apple product names (Xcode, xcodebuild, Apple ID, visionOS) read fine
// unbackticked throughout this module's docs.
#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};

/// The throwaway app's name (its `.app` / target / scheme stem).
pub const BOOTSTRAP_APP: &str = "IdtxSigningBootstrap";

/// Errors from the signing bootstrap.
#[derive(Debug, thiserror::Error)]
pub enum VisionOsBootstrapError {
    /// The bootstrap shell-out is macOS-only and this host is not macOS.
    #[error("visionOS signing bootstrap is only available on macOS")]
    NotMacOs,
    /// Generating the throwaway project failed (I/O).
    #[error("generating the signing-bootstrap project at {path} failed: {source}")]
    Generate {
        /// The scratch directory being written.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// `xcodebuild` could not be spawned (toolchain not selected?).
    #[error("failed to spawn `xcodebuild`: {0}")]
    Spawn(std::io::Error),
    /// `xcodebuild` ran but exited non-zero (free teams usually need the one-time
    /// Xcode GUI mint). `xcodeproj` points at the generated project when it was
    /// left on disk (`cleanup = false`), so a CLI caller can open it in Xcode.
    #[error("`xcodebuild` could not mint credentials for `{bundle_id}` ({status})")]
    XcodebuildFailed {
        /// The bundle id that was being provisioned.
        bundle_id: String,
        /// The exit status.
        status: String,
        /// The generated `.xcodeproj`, when left on disk for a GUI retry.
        xcodeproj: Option<PathBuf>,
    },
}

/// What to provision, and where to stage the throwaway project.
#[derive(Debug, Clone)]
pub struct SigningBootstrapPlan<'a> {
    /// Bundle id to provision — pass the app's **own** id so the minted profile
    /// maps to it (`PRODUCT_BUNDLE_IDENTIFIER`).
    pub bundle_id: &'a str,
    /// Signing team id (10-char Team ID). `None` lets automatic signing resolve
    /// the single registered account.
    pub team: Option<&'a str>,
    /// Target device UDID — the paired Vision Pro to register.
    pub device_udid: &'a str,
    /// Where the throwaway `.xcodeproj` is generated. Created fresh.
    pub scratch_dir: &'a Path,
    /// Remove `scratch_dir` when done (both on success and failure) — the
    /// "creation → mint → deletion" packaging path. `false` leaves the project
    /// on disk on failure so a CLI caller can open it in Xcode.
    pub cleanup: bool,
}

/// Generate the throwaway project, run `xcodebuild` to mint the cert + profile +
/// register the device, then (when `plan.cleanup`) remove the scratch dir.
///
/// # Errors
/// [`VisionOsBootstrapError`] on a non-macOS host, an I/O failure generating the
/// project, or a failed `xcodebuild` (free teams typically still need the
/// one-time Xcode GUI mint).
#[cfg(target_os = "macos")]
pub fn bootstrap_signing(plan: &SigningBootstrapPlan<'_>) -> Result<(), VisionOsBootstrapError> {
    let xcodeproj =
        generate_bootstrap_project(plan.scratch_dir, plan.bundle_id).map_err(|source| {
            VisionOsBootstrapError::Generate {
                path: plan.scratch_dir.to_path_buf(),
                source,
            }
        })?;

    let args = bootstrap_xcodebuild_args(&xcodeproj, plan.team, plan.bundle_id, plan.device_udid);
    let status = std::process::Command::new("xcodebuild")
        .args(&args)
        .status()
        .map_err(VisionOsBootstrapError::Spawn)?;

    // The "creation → mint → deletion": remove the scratch project when asked,
    // regardless of outcome. Best-effort — a cleanup failure never masks the real
    // result.
    if plan.cleanup {
        let _ = std::fs::remove_dir_all(plan.scratch_dir);
    }

    if status.success() {
        Ok(())
    } else {
        Err(VisionOsBootstrapError::XcodebuildFailed {
            bundle_id: plan.bundle_id.to_owned(),
            status: status.to_string(),
            // Only surface the path when we left it on disk for a GUI retry.
            xcodeproj: if plan.cleanup { None } else { Some(xcodeproj) },
        })
    }
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn bootstrap_signing(_plan: &SigningBootstrapPlan<'_>) -> Result<(), VisionOsBootstrapError> {
    Err(VisionOsBootstrapError::NotMacOs)
}

/// The `xcodebuild` arg vector that mints the cert/profile + registers the device.
/// Signing-relevant settings are passed as CLI overrides so they win over the
/// generated project. `team` is omitted when `None`, letting automatic signing
/// resolve the single account.
#[must_use]
pub fn bootstrap_xcodebuild_args(
    xcodeproj: &Path,
    team: Option<&str>,
    bundle_id: &str,
    device_udid: &str,
) -> Vec<String> {
    let mut args = vec![
        "-project".to_owned(),
        xcodeproj.display().to_string(),
        "-scheme".to_owned(),
        BOOTSTRAP_APP.to_owned(),
        "-destination".to_owned(),
        format!("platform=visionOS,id={device_udid}"),
        "-allowProvisioningUpdates".to_owned(),
        "-allowProvisioningDeviceRegistration".to_owned(),
        "CODE_SIGN_STYLE=Automatic".to_owned(),
    ];
    if let Some(team) = team {
        args.push(format!("DEVELOPMENT_TEAM={team}"));
    }
    args.push(format!("PRODUCT_BUNDLE_IDENTIFIER={bundle_id}"));
    args.push("build".to_owned());
    args
}

/// Whether `s` looks like an Apple Team ID: exactly 10 uppercase-alphanumeric
/// characters (not an Apple ID email).
#[must_use]
pub fn looks_like_team_id(s: &str) -> bool {
    s.len() == 10
        && s.chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
}

/// Auto-detect the free Personal Team's id from Xcode's registered accounts
/// (`defaults read com.apple.dt.Xcode IDEProvisioningTeamByIdentifier`), preferring
/// a free provisioning team. macOS only.
#[cfg(target_os = "macos")]
#[must_use]
pub fn detect_free_team() -> Option<String> {
    let out = std::process::Command::new("defaults")
        .args([
            "read",
            "com.apple.dt.Xcode",
            "IDEProvisioningTeamByIdentifier",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_free_team_id(&String::from_utf8_lossy(&out.stdout))
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn detect_free_team() -> Option<String> {
    None
}

/// Parse `defaults read … IDEProvisioningTeamByIdentifier` output for a Team ID,
/// preferring the one flagged `isFreeProvisioningTeam = 1`. Values are unquoted in
/// that output (e.g. `teamID = SHD387D78J;`), with the free flag preceding the id
/// within each team block.
#[must_use]
pub fn parse_free_team_id(output: &str) -> Option<String> {
    let mut teams: Vec<(String, bool)> = Vec::new();
    let mut pending_free = false;
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with("isFreeProvisioningTeam") && line.contains("= 1") {
            pending_free = true;
        } else if let Some(rest) = line.strip_prefix("teamID = ") {
            let id = rest
                .trim()
                .trim_end_matches(';')
                .trim_matches('"')
                .to_owned();
            if !id.is_empty() {
                teams.push((id, pending_free));
            }
            pending_free = false;
        }
    }
    teams
        .iter()
        .find(|(_, free)| *free)
        .or_else(|| teams.first())
        .map(|(id, _)| id.clone())
}

/// Generate the throwaway `.xcodeproj` (+ `BootstrapApp.swift` + shared scheme)
/// under `proj_dir`, keyed to `bundle_id`. Any pre-existing `proj_dir` is cleared
/// first. Returns the path to the generated `.xcodeproj`.
///
/// # Errors
/// Propagates any filesystem error writing the scratch project.
pub fn generate_bootstrap_project(proj_dir: &Path, bundle_id: &str) -> std::io::Result<PathBuf> {
    let xcodeproj = proj_dir.join(format!("{BOOTSTRAP_APP}.xcodeproj"));
    let scheme_dir = xcodeproj.join("xcshareddata").join("xcschemes");
    if proj_dir.exists() {
        std::fs::remove_dir_all(proj_dir)?;
    }
    std::fs::create_dir_all(&scheme_dir)?;

    std::fs::write(proj_dir.join("BootstrapApp.swift"), MAIN_SWIFT)?;
    std::fs::write(xcodeproj.join("project.pbxproj"), pbxproj(bundle_id))?;
    std::fs::write(
        scheme_dir.join(format!("{BOOTSTRAP_APP}.xcscheme")),
        xcscheme(),
    )?;
    Ok(xcodeproj)
}

/// The project file, with the bundle id substituted in (quoted, so dotted ids are
/// always valid pbxproj). The team + bundle id are also passed as `xcodebuild` CLI
/// overrides; `DEVELOPMENT_TEAM` is deliberately NOT baked into the project (it
/// would corrupt the pbxproj if a non-identifier value slipped in, and automatic
/// signing resolves the account at build time).
fn pbxproj(bundle_id: &str) -> String {
    PBXPROJ_TEMPLATE
        .replace("{APP}", BOOTSTRAP_APP)
        .replace("{BUNDLE}", bundle_id)
}

/// A shared scheme so `xcodebuild -scheme` resolves (and `-destination` applies).
fn xcscheme() -> String {
    XCSCHEME_TEMPLATE.replace("{APP}", BOOTSTRAP_APP)
}

/// A trivial SwiftUI visionOS app — enough to compile + sign (never run).
const MAIN_SWIFT: &str = r#"import SwiftUI

@main
struct IdtxSigningBootstrapApp: App {
    var body: some Scene {
        WindowGroup { Text("IDTX signing bootstrap") }
    }
}
"#;

const PBXPROJ_TEMPLATE: &str = r#"// !$*UTF8*$!
{
	archiveVersion = 1;
	classes = {
	};
	objectVersion = 56;
	objects = {
		1D0000000000000000000005 /* BootstrapApp.swift in Sources */ = {isa = PBXBuildFile; fileRef = 1D0000000000000000000004 /* BootstrapApp.swift */; };
		1D0000000000000000000003 /* {APP}.app */ = {isa = PBXFileReference; explicitFileType = wrapper.application; includeInIndex = 0; path = "{APP}.app"; sourceTree = BUILT_PRODUCTS_DIR; };
		1D0000000000000000000004 /* BootstrapApp.swift */ = {isa = PBXFileReference; lastKnownFileType = sourcecode.swift; path = BootstrapApp.swift; sourceTree = "<group>"; };
		1D0000000000000000000007 /* Frameworks */ = {isa = PBXFrameworksBuildPhase; buildActionMask = 2147483647; files = (); runOnlyForDeploymentPostprocessing = 0; };
		1D0000000000000000000009 = {isa = PBXGroup; children = (1D0000000000000000000004 /* BootstrapApp.swift */, 1D000000000000000000000A /* Products */, ); sourceTree = "<group>"; };
		1D000000000000000000000A /* Products */ = {isa = PBXGroup; children = (1D0000000000000000000003 /* {APP}.app */, ); name = Products; sourceTree = "<group>"; };
		1D0000000000000000000002 /* {APP} */ = {isa = PBXNativeTarget; buildConfigurationList = 1D000000000000000000000C /* Build configuration list for PBXNativeTarget "{APP}" */; buildPhases = (1D0000000000000000000006 /* Sources */, 1D0000000000000000000007 /* Frameworks */, ); buildRules = (); dependencies = (); name = "{APP}"; productName = "{APP}"; productReference = 1D0000000000000000000003 /* {APP}.app */; productType = "com.apple.product-type.application"; };
		1D0000000000000000000001 /* Project object */ = {isa = PBXProject; attributes = {BuildIndependentTargetsInParallel = 1; LastSwiftUpdateCheck = 1500; LastUpgradeCheck = 1500; TargetAttributes = {1D0000000000000000000002 = {CreatedOnToolsVersion = 15.0; }; }; }; buildConfigurationList = 1D000000000000000000000B /* Build configuration list for PBXProject "{APP}" */; compatibilityVersion = "Xcode 14.0"; developmentRegion = en; hasScannedForEncodings = 0; knownRegions = (en, Base, ); mainGroup = 1D0000000000000000000009; productRefGroup = 1D000000000000000000000A /* Products */; projectDirPath = ""; projectRoot = ""; targets = (1D0000000000000000000002 /* {APP} */, ); };
		1D0000000000000000000006 /* Sources */ = {isa = PBXSourcesBuildPhase; buildActionMask = 2147483647; files = (1D0000000000000000000005 /* BootstrapApp.swift in Sources */, ); runOnlyForDeploymentPostprocessing = 0; };
		1D000000000000000000000D /* Debug */ = {isa = XCBuildConfiguration; buildSettings = {SWIFT_VERSION = 5.0; }; name = Debug; };
		1D000000000000000000000E /* Release */ = {isa = XCBuildConfiguration; buildSettings = {SWIFT_VERSION = 5.0; }; name = Release; };
		1D000000000000000000000F /* Debug */ = {isa = XCBuildConfiguration; buildSettings = {CODE_SIGN_STYLE = Automatic; CURRENT_PROJECT_VERSION = 1; GENERATE_INFOPLIST_FILE = YES; MARKETING_VERSION = 1.0; PRODUCT_BUNDLE_IDENTIFIER = "{BUNDLE}"; PRODUCT_NAME = "$(TARGET_NAME)"; SDKROOT = xros; SUPPORTED_PLATFORMS = "xros xrsimulator"; SWIFT_VERSION = 5.0; TARGETED_DEVICE_FAMILY = 7; XROS_DEPLOYMENT_TARGET = 2.0; }; name = Debug; };
		1D0000000000000000000010 /* Release */ = {isa = XCBuildConfiguration; buildSettings = {CODE_SIGN_STYLE = Automatic; CURRENT_PROJECT_VERSION = 1; GENERATE_INFOPLIST_FILE = YES; MARKETING_VERSION = 1.0; PRODUCT_BUNDLE_IDENTIFIER = "{BUNDLE}"; PRODUCT_NAME = "$(TARGET_NAME)"; SDKROOT = xros; SUPPORTED_PLATFORMS = "xros xrsimulator"; SWIFT_VERSION = 5.0; TARGETED_DEVICE_FAMILY = 7; XROS_DEPLOYMENT_TARGET = 2.0; }; name = Release; };
		1D000000000000000000000B /* Build configuration list for PBXProject "{APP}" */ = {isa = XCConfigurationList; buildConfigurations = (1D000000000000000000000D /* Debug */, 1D000000000000000000000E /* Release */, ); defaultConfigurationIsVisible = 0; defaultConfigurationName = Release; };
		1D000000000000000000000C /* Build configuration list for PBXNativeTarget "{APP}" */ = {isa = XCConfigurationList; buildConfigurations = (1D000000000000000000000F /* Debug */, 1D0000000000000000000010 /* Release */, ); defaultConfigurationIsVisible = 0; defaultConfigurationName = Release; };
	};
	rootObject = 1D0000000000000000000001 /* Project object */;
}
"#;

const XCSCHEME_TEMPLATE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Scheme LastUpgradeVersion = "1500" version = "1.7">
   <BuildAction parallelizeBuildables = "YES" buildImplicitDependencies = "YES">
      <BuildActionEntries>
         <BuildActionEntry buildForTesting = "YES" buildForRunning = "YES" buildForProfiling = "YES" buildForArchiving = "YES" buildForAnalyzing = "YES">
            <BuildableReference
               BuildableIdentifier = "primary"
               BlueprintIdentifier = "1D0000000000000000000002"
               BuildableName = "{APP}.app"
               BlueprintName = "{APP}"
               ReferencedContainer = "container:{APP}.xcodeproj">
            </BuildableReference>
         </BuildActionEntry>
      </BuildActionEntries>
   </BuildAction>
   <LaunchAction buildConfiguration = "Debug" selectedDebuggerIdentifier = "Xcode.DebuggerFoundation.Debugger.LLDB" selectedLauncherIdentifier = "Xcode.DebuggerFoundation.Launcher.LLDB" launchStyle = "0" useCustomWorkingDirectory = "NO" ignoresPersistentStateOnLaunch = "NO" debugDocumentVersioning = "YES" debugServiceExtension = "internal" allowLocationSimulation = "YES">
   </LaunchAction>
</Scheme>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_id_shape() {
        assert!(looks_like_team_id("ABCDE12345"));
        assert!(looks_like_team_id("SHD387D78J"));
        assert!(!looks_like_team_id("a.borrmann@sap.com")); // an Apple ID, not a team id
        assert!(!looks_like_team_id("abcde12345")); // lowercase
        assert!(!looks_like_team_id("ABCDE1234")); // 9 chars
    }

    #[test]
    fn parse_free_team_prefers_free_and_handles_unquoted() {
        // Xcode 26 shape: unquoted teamID, free flag precedes it.
        let one = "{\n    \"UUID\" = (\n        {\n            isFreeProvisioningTeam = 1;\n            teamID = SHD387D78J;\n            teamName = \"Andre Borrmann (Personal Team)\";\n        }\n    );\n}";
        assert_eq!(parse_free_team_id(one).as_deref(), Some("SHD387D78J"));
        // Two teams: the free one wins even if listed second.
        let two = "{\n  {\n    teamID = AAAA000000;\n  },\n  {\n    isFreeProvisioningTeam = 1;\n    teamID = BBBB111111;\n  }\n}";
        assert_eq!(parse_free_team_id(two).as_deref(), Some("BBBB111111"));
        // No free flag → first team.
        let none = "{\n  (\n    {\n      teamID = CCCC222222;\n    }\n  )\n}";
        assert_eq!(parse_free_team_id(none).as_deref(), Some("CCCC222222"));
        assert_eq!(parse_free_team_id("{}"), None);
    }

    #[test]
    fn xcodebuild_args_mint_provisioning() {
        let args = bootstrap_xcodebuild_args(
            Path::new("/p/IdtxSigningBootstrap.xcodeproj"),
            Some("TEAMID1234"),
            "com.sap.apeirora.idtx.vision",
            "UDID-1",
        );
        assert!(args.contains(&"-allowProvisioningUpdates".to_owned()));
        assert!(args.contains(&"-allowProvisioningDeviceRegistration".to_owned()));
        assert!(args.contains(&"DEVELOPMENT_TEAM=TEAMID1234".to_owned()));
        assert!(
            args.contains(&"PRODUCT_BUNDLE_IDENTIFIER=com.sap.apeirora.idtx.vision".to_owned())
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-destination" && w[1] == "platform=visionOS,id=UDID-1")
        );
        assert!(args.contains(&"build".to_owned()));
        // Without a team id, DEVELOPMENT_TEAM is omitted (automatic signing resolves it).
        let no_team =
            bootstrap_xcodebuild_args(Path::new("/p/x.xcodeproj"), None, "b.id", "UDID-2");
        assert!(!no_team.iter().any(|a| a.starts_with("DEVELOPMENT_TEAM")));
    }

    #[test]
    fn pbxproj_substitutes_bundle_and_is_team_free() {
        let p = pbxproj("com.example.demo");
        // Bundle id is quoted (dotted ids are always valid pbxproj).
        assert!(p.contains("PRODUCT_BUNDLE_IDENTIFIER = \"com.example.demo\";"));
        assert!(p.contains("SDKROOT = xros;"));
        // The team is never baked into the project (it would corrupt the pbxproj).
        assert!(!p.contains("DEVELOPMENT_TEAM"));
        assert!(!p.contains("{BUNDLE}") && !p.contains("{TEAM}") && !p.contains("{APP}"));
    }

    #[test]
    fn generate_writes_project_tree() {
        let tmp = std::env::temp_dir().join(format!("idtx-bootstrap-test-{}", std::process::id()));
        let xcodeproj =
            generate_bootstrap_project(&tmp, "com.sap.apeirora.idtx.vision").expect("generate");
        assert!(xcodeproj.join("project.pbxproj").is_file());
        assert!(tmp.join("BootstrapApp.swift").is_file());
        assert!(
            xcodeproj
                .join("xcshareddata/xcschemes")
                .join(format!("{BOOTSTRAP_APP}.xcscheme"))
                .is_file()
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
