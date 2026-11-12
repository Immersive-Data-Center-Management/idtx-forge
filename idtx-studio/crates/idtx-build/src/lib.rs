//! Confined, reproducible desktop/PCVR cook and package pipeline.
//!
//! `idtx-build` is the single library that turns an authored Conductor project
//! into a launchable experience. It has two layers:
//!
//! * **Cook** ([`cook`]) — target-agnostic. Flattens + localizes the entry
//!   stage, collects + validates behavior modules, and emits a stamped schema-v2
//!   `run.toml` with a reproducibility hash. Produces a portable *payload*.
//! * **Package** ([`Packager`]) — per-target. Wraps a cooked payload with the
//!   runtime binary and platform scaffolding (macOS `.app`, Windows folder,
//!   Linux bundle) for a concrete [`CookTarget`].
//!
//! Both `cargo xtask` and the Conductor "Package" command call this one library,
//! so the mac/win/linux bundling logic lives in exactly one place.

mod apk;
mod apple_identity;
mod apple_tools;
#[cfg(feature = "cooking")]
pub mod assembly;
mod cook;
mod devicectl;
mod package;
mod publication;
mod signing;
mod steamvr;
mod usd_runtime;
mod usdz_metadata;
mod visionos_assemble;
mod visionos_bootstrap;

pub use apk::{
    ANDROID_MIN_SDK, ANDROID_TARGET_SDK, AndroidSdk, ApkAssembler, ApkError, ApkSigning,
    aapt2_link_args, apksigner_sign_args, apksigner_verify_args, jar_update_args,
    keytool_debug_args, zipalign_args,
};
pub use apple_identity::{
    IdentityError, MobileProvision, default_profiles_dir, discover_development_identity,
    discover_profile_for, parse_identities, parse_mobileprovision_plist, pick_development_identity,
    profile_search_dirs, read_profile, select_profile,
};
pub use apple_tools::{
    AppleToolError, add_rpath, change_install_name, codesign_adhoc, codesign_with_identity,
    delete_rpath, xcrun_find_tool, xcrun_show_sdk_path,
};
#[cfg(feature = "cooking")]
pub use cook::{AssemblyArtifact, author_cook_assembly};
pub use cook::{
    BehaviorArtifact, CookError, CookPlan, CookReport, CookTarget, LevelBehaviorOverlay,
    LevelBehaviorPrepare, LevelBehaviorPrepareFn, PAYLOAD_BEHAVIORS_DIR, PAYLOAD_MANIFEST,
    PAYLOAD_STAGE, PIN_GLAM, PIN_WASMTIME, PIN_WGPU, cook,
};
pub use devicectl::{
    DeviceCtlError, DeviceInfo, copy_from, copy_from_args, install_app, install_app_args,
    launch_app, launch_args, list_devices_args, list_paired_devices, parse_devices_json,
    select_visionos_device,
};
pub use package::{
    ANDROID_ABI_DIR, AndroidApkPackager, BUNDLE_MANIFEST, BundleKind, DesktopFolderPackager,
    MacAppInfo, MacAppPackager, RuntimeBinary, VisionOsAppPackager, VisionOsPlistOptions,
    bundle_kind_for, empty_plug_info_library_path, visionos_info_plist, visionos_info_plist_with,
};
pub use signing::{SigningConfig, SigningError, SigningPlan};
pub use steamvr::{
    ACTION_MANIFEST_FILE, Action, ActionSet, default_action_set, render_action_manifest,
};
pub use usd_runtime::{UsdStageReport, stage_standalone_usd_runtime, stage_usd_runtime};
pub use visionos_assemble::{
    DeviceAppPlan, VisionOsAssembleError, c_header, embed_dylib_closure, finish_device_app,
    is_dylib_name, swift_shell_source, swiftc_args, visionos_entitlements_plist,
};
pub use visionos_bootstrap::{
    BOOTSTRAP_APP, SigningBootstrapPlan, VisionOsBootstrapError, bootstrap_signing,
    bootstrap_xcodebuild_args, detect_free_team, generate_bootstrap_project, looks_like_team_id,
    parse_free_team_id,
};

use std::path::PathBuf;

/// Wraps a cooked payload into a final, launchable bundle for one target.
///
/// This is the per-target seam: the [`cook`] step produces a payload that
/// is identical across targets, and a `Packager` implementation co-locates the
/// runtime binary and any platform scaffolding for its [`CookTarget`]. Concrete
/// implementations (macOS `.app`, Windows folder, Linux bundle) are provided by
/// the packaging layer; `cargo xtask` and the Conductor "Package" command both
/// drive them, so there is no second copy of the bundling logic.
pub trait Packager {
    /// The target this packager builds for.
    fn target(&self) -> &CookTarget;

    /// Wrap `report`'s payload into a launchable bundle, returning the bundle
    /// root path.
    ///
    /// # Errors
    ///
    /// Returns [`CookError`] if the bundle cannot be assembled (I/O, a missing
    /// runtime binary, …).
    fn package(&self, report: &CookReport) -> Result<PathBuf, CookError>;
}

#[cfg(test)]
mod tests {
    use idtx_runtime_manifest::NetworkConfig;

use super::*;

    #[test]
    fn cook_target_presets() {
        let d = CookTarget::desktop("x86_64-pc-windows-msvc");
        assert_eq!(d.mode, "desktop");
        assert!(d.features.is_empty());
        // Packaged desktop runs fullscreen + captures the cursor.
        assert!(d.fullscreen);

        let x = CookTarget::pcvr("x86_64-pc-windows-msvc");
        assert_eq!(x.mode, "xr");
        assert_eq!(x.features, vec!["pcvr".to_string()]);
        // XR targets let the compositor own presentation — no desktop fullscreen.
        assert!(!x.fullscreen);

        // Linux PCVR: same cook features as pcvr, distinct label so a
        // packager can select the SteamVR/Monado loader path.
        let l = CookTarget::pcvr_linux("x86_64-unknown-linux-gnu");
        assert_eq!(l.mode, "xr");
        assert_eq!(l.label, "linux-pcvr");
        assert_eq!(l.features, vec!["pcvr".to_string()]);
    }

    #[test]
    fn cooked_features_merge_and_dedupe() {
        let plan = CookPlan {
            entry_stage: PathBuf::from("x.usda"),
            name: "N".to_string(),
            asset_roots: Vec::new(),
            target: CookTarget {
                triple: "t".to_string(),
                label: "l".to_string(),
                mode: "xr".to_string(),
                fullscreen: false,
                features: vec!["pcvr".to_string(), "shared".to_string()],
            },
            extra_features: vec!["shared".to_string(), "extra".to_string()],
            xr: idtx_runtime_manifest::XrConfig::default(),
            out_dir: PathBuf::from("out"),
            project_root: PathBuf::from("proj"),
            levels: Vec::new(),
            default_level: None,
            start_level: None,
            transfer_level: None,
            shared_sublayers: Vec::new(),
            player: None,
            entry_override: None,
            loose_levels: false,
            network: NetworkConfig::default(),
            embed_dev_token: None,
            texture_quality: None,
            level_behavior_prepare: None,
        };
        assert_eq!(plan.cooked_features(), vec!["extra", "pcvr", "shared"]);
    }
}
