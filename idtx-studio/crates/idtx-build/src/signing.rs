//! Code-signing / notarization configuration surface (scaffold + doc).
//!
//! Signing a packaged experience is **platform- and credential-bound** and cannot
//! be exercised in this environment (no Apple Developer identity / `notarytool`
//! session on this Windows host, and no Authenticode certificate + timestamp
//! service wired into CI). This module therefore lands the *configuration surface*
//! a signer consumes — the typed inputs each platform needs and a dry-run
//! [`SigningPlan::describe`] — while the actual `codesign`/`xcrun notarytool`/
//! `signtool` invocations are deferred to a host with real credentials.
//!
//! The seam is placed so that the deferred work is a *fill-in*, not a redesign:
//! [`Packager`](crate::Packager) implementations gain an optional
//! [`SigningConfig`] and, once credentials exist, call [`SigningPlan::execute`]
//! (currently a documented [`SigningError::NotImplemented`]) after laying out the
//! bundle.
//!
//! ## What remains (per platform)
//!
//! * **macOS** — `codesign --deep --options runtime --sign <identity>` over the
//!   `.app`, then `xcrun notarytool submit --wait` + `xcrun stapler staple`. Needs
//!   a Developer ID Application certificate in the login keychain and an App Store
//!   Connect API key or notary-profile.
//! * **Windows** — `signtool sign /fd SHA256 /tr <timestamp-url> /td SHA256`
//!   against the runtime `.exe` (and, if produced, an MSIX/NSIS installer). Needs
//!   an Authenticode certificate (thumbprint in the machine store, or a `.pfx` +
//!   password) and a timestamp authority URL.
//! * **Linux** — no OS-level binary signing is required to run; distribution
//!   integrity is a detached GPG signature over the tarball. Optional, so the
//!   Linux variant carries no mandatory signing input.

// Docs name platform signing tools / Apple identifiers (notarytool, ARKit,
// MTLDevice, SigningPlan::execute) that read fine unbackticked.
#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};

/// Per-target signing inputs. Each variant carries only what its platform's
/// signing toolchain needs; [`SigningConfig::None`] is an explicit unsigned build.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SigningConfig {
    /// Unsigned — the default, and the only mode reachable without credentials.
    #[default]
    None,
    /// macOS Developer ID signing + notarization.
    MacOs {
        /// `codesign --sign` identity (e.g. `"Developer ID Application: Acme (TEAMID)"`).
        identity: String,
        /// `xcrun notarytool --keychain-profile` name, if notarization is wanted.
        notary_profile: Option<String>,
    },
    /// Apple Vision Pro (`.app`) signing. Two modes:
    ///
    /// * **Store / notarized** (`development = false`, the macOS-identical path):
    ///   `codesign --deep --options runtime [--entitlements]` + optional
    ///   `notarytool`/`stapler`. For Developer-ID distribution.
    /// * **Device development** (`development = true`): the sequence a physical
    ///   Vision Pro actually accepts — inner `Frameworks/*.dylib` are signed
    ///   first, then the bundle with `--entitlements` (ARKit usage strings live
    ///   in Info.plist; the entitlements plist carries `get-task-allow` +
    ///   `application-identifier`), **without** `--deep` and **without**
    ///   `--options runtime` (a macOS hardened-runtime/notarization flag the
    ///   device rejects), and **never** notarized. Works with a free
    ///   "Personal Team" `Apple Development` identity; the device profile is
    ///   embedded as `embedded.mobileprovision` by the packager before signing.
    VisionOs {
        /// `codesign --sign` identity (`Apple Development: NAME (TEAMID)` for a
        /// free team, a `Developer ID` cert for store, or `"-"` ad-hoc for the
        /// Simulator).
        identity: String,
        /// Path to the entitlements plist, passed to `codesign --entitlements`.
        /// `None` signs without extra entitlements (e.g. an ad-hoc Simulator
        /// build).
        entitlements: Option<PathBuf>,
        /// `xcrun notarytool --keychain-profile` name, if notarization is wanted
        /// (store distribution only; never set for `development`).
        notary_profile: Option<String>,
        /// Path to the development provisioning profile embedded in the `.app`
        /// (device development only; informational for [`SigningPlan::describe`]
        /// — the packager performs the copy).
        provisioning_profile: Option<PathBuf>,
        /// Select the **device development** signing sequence (no `--options
        /// runtime`, inner dylibs signed first, no notarization).
        development: bool,
    },
    /// Windows Authenticode signing.
    Windows {
        /// Certificate thumbprint in the machine/user store, or a path to a `.pfx`.
        certificate: String,
        /// RFC-3161 timestamp authority URL (e.g. `"http://timestamp.digicert.com"`).
        timestamp_url: Option<String>,
    },
    /// Linux detached-signature (optional; distribution integrity only).
    Linux {
        /// GPG key id to sign the distribution tarball with.
        gpg_key_id: String,
    },
    /// Android `.apk` signing via `apksigner` (Quest standalone).
    Android {
        /// Path to the Java keystore (`.jks` / `.keystore`) holding the signing key.
        keystore: String,
        /// Key alias within the keystore to sign with.
        key_alias: String,
    },
}

impl SigningConfig {
    /// Whether this configuration would sign anything.
    #[must_use]
    pub const fn is_signed(&self) -> bool {
        !matches!(self, SigningConfig::None)
    }

    /// Whether this configuration additionally notarizes (Apple only).
    #[must_use]
    pub const fn is_notarized(&self) -> bool {
        matches!(
            self,
            SigningConfig::MacOs {
                notary_profile: Some(_),
                ..
            } | SigningConfig::VisionOs {
                notary_profile: Some(_),
                ..
            }
        )
    }

    /// Whether this is a visionOS **device development** signing config (the free
    /// Personal-Team path: no hardened runtime, no notarization).
    #[must_use]
    pub const fn is_visionos_development(&self) -> bool {
        matches!(
            self,
            SigningConfig::VisionOs {
                development: true,
                ..
            }
        )
    }
}

/// Errors from signing execution.
#[derive(Debug, thiserror::Error)]
pub enum SigningError {
    /// The signing execution is not implemented for this platform/host. Carries
    /// the human-readable command sequence a credentialed host must run (e.g. a
    /// Windows/Linux/Android config, or an Apple config on a non-macOS host).
    #[error("signing not implemented in this environment; required steps:\n{0}")]
    NotImplemented(String),
    /// A signing tool could not be spawned (not on `PATH`).
    #[error("failed to spawn `{tool}`: {source}")]
    Spawn {
        /// The tool that failed to launch.
        tool: String,
        /// The underlying spawn error.
        source: std::io::Error,
    },
    /// A signing tool ran but exited non-zero.
    #[error("`{tool}` failed ({status}): {stderr}")]
    CommandFailed {
        /// The tool that failed.
        tool: String,
        /// Its exit status.
        status: String,
        /// Captured stderr (trimmed).
        stderr: String,
    },
}

/// The `notarytool submit` + `stapler staple` command suffix for a configured
/// keychain profile, or empty when notarization is off. Shared by the macOS and
/// visionOS store-signing describe branches.
fn notary_suffix(bundle: &str, notary_profile: Option<&str>) -> String {
    notary_profile.map_or_else(String::new, |profile| {
        format!(
            "\nxcrun notarytool submit \"{bundle}\" --keychain-profile \"{profile}\" --wait\
             \nxcrun stapler staple \"{bundle}\""
        )
    })
}

/// A resolved signing plan over a concrete bundle path.
///
/// [`describe`](Self::describe) renders the exact command sequence (a dry run that
/// works everywhere); [`execute`](Self::execute) is the credential-bound seam,
/// deferred to a host-side follow-up.
#[derive(Debug, Clone)]
pub struct SigningPlan<'a> {
    /// The signing inputs.
    pub config: &'a SigningConfig,
    /// The bundle root to sign (`.app`, folder, or installer).
    pub bundle: &'a Path,
}

impl<'a> SigningPlan<'a> {
    /// Build a plan.
    #[must_use]
    pub fn new(config: &'a SigningConfig, bundle: &'a Path) -> Self {
        Self { config, bundle }
    }

    /// Render the platform command sequence this plan *would* run. Pure and
    /// credential-free, so it is testable and can be surfaced in the UI / CI log.
    #[must_use]
    pub fn describe(&self) -> String {
        let b = self.bundle.display();
        match self.config {
            SigningConfig::None => format!("unsigned build: {b} (no signing configured)"),
            SigningConfig::MacOs {
                identity,
                notary_profile,
            } => {
                let notary = notary_suffix(&b.to_string(), notary_profile.as_deref());
                format!(
                    "codesign --deep --force --options runtime --sign \"{identity}\" \"{b}\"{notary}"
                )
            }
            SigningConfig::VisionOs {
                identity,
                entitlements,
                notary_profile,
                provisioning_profile,
                development,
            } => {
                let ents = entitlements
                    .as_ref()
                    .map(|p| format!(" --entitlements \"{}\"", p.display()))
                    .unwrap_or_default();
                if *development {
                    // Device development: embed the profile, sign inner dylibs
                    // first, then the bundle — NO `--options runtime`, NO deep,
                    // NO notarization. This is what a physical Vision Pro accepts.
                    let prov = provisioning_profile
                        .as_ref()
                        .map(|p| {
                            format!("cp \"{}\" \"{b}/embedded.mobileprovision\"\n", p.display())
                        })
                        .unwrap_or_default();
                    format!(
                        "{prov}\
                         codesign --force --sign \"{identity}\" \"{b}\"/Frameworks/*.dylib\n\
                         codesign --force{ents} --sign \"{identity}\" \"{b}\""
                    )
                } else {
                    let notary = notary_suffix(&b.to_string(), notary_profile.as_deref());
                    format!(
                        "codesign --deep --force --options runtime{ents} --sign \"{identity}\" \"{b}\"{notary}"
                    )
                }
            }
            SigningConfig::Windows {
                certificate,
                timestamp_url,
            } => {
                let ts = timestamp_url
                    .as_deref()
                    .map(|u| format!(" /tr \"{u}\" /td SHA256"))
                    .unwrap_or_default();
                format!("signtool sign /fd SHA256 /sha1 \"{certificate}\"{ts} \"{b}\"")
            }
            SigningConfig::Linux { gpg_key_id } => {
                format!("gpg --default-key \"{gpg_key_id}\" --detach-sign --armor \"{b}\"")
            }
            SigningConfig::Android {
                keystore,
                key_alias,
            } => {
                // The `.apk` is produced by `gradle assembleRelease`; apksigner
                // then signs the unsigned release artifact in-place.
                format!(
                    "apksigner sign --ks \"{keystore}\" --ks-key-alias \"{key_alias}\" \"{b}\"\
                     \napksigner verify \"{b}\""
                )
            }
        }
    }

    /// Execute the signing plan.
    ///
    /// Apple configurations ([`SigningConfig::MacOs`] / [`SigningConfig::VisionOs`])
    /// are executed for real on a **macOS** host: `codesign` (with `--entitlements`
    /// for the immersive visionOS app), then — when a notary profile is set —
    /// `xcrun notarytool submit --wait` + `xcrun stapler staple`. The identity may
    /// be a Developer ID / Apple Development certificate, or `"-"` for an ad-hoc
    /// signature (what the Simulator accepts). On a non-macOS host, or for the
    /// Windows/Linux/Android configs (whose toolchains are not driven here), it
    /// returns [`SigningError::NotImplemented`] carrying [`describe`](Self::describe).
    /// An unsigned ([`SigningConfig::None`]) plan is a no-op success.
    ///
    /// # Errors
    ///
    /// [`SigningError::Spawn`] / [`SigningError::CommandFailed`] when a signing
    /// tool is missing or fails; [`SigningError::NotImplemented`] for configs this
    /// host cannot execute.
    pub fn execute(&self) -> Result<(), SigningError> {
        match self.config {
            SigningConfig::None => Ok(()),
            SigningConfig::MacOs { .. } | SigningConfig::VisionOs { .. } => {
                #[cfg(target_os = "macos")]
                {
                    self.execute_apple()
                }
                #[cfg(not(target_os = "macos"))]
                {
                    Err(SigningError::NotImplemented(self.describe()))
                }
            }
            // Windows/Linux/Android signing toolchains are not driven from this
            // crate (the APK path signs via `idtx-build::apk`); surface the steps.
            _ => Err(SigningError::NotImplemented(self.describe())),
        }
    }

    /// Run `codesign` (+ optional `notarytool`/`stapler`) for an Apple config.
    #[cfg(target_os = "macos")]
    fn execute_apple(&self) -> Result<(), SigningError> {
        use std::process::Command;

        let bundle = self.bundle.display().to_string();
        let (identity, entitlements, notary, development) = match self.config {
            SigningConfig::MacOs {
                identity,
                notary_profile,
            } => (identity.as_str(), None, notary_profile.as_deref(), false),
            SigningConfig::VisionOs {
                identity,
                entitlements,
                notary_profile,
                development,
                ..
            } => (
                identity.as_str(),
                entitlements.as_deref(),
                notary_profile.as_deref(),
                *development,
            ),
            _ => unreachable!("execute_apple only called for Apple configs"),
        };

        let run =
            |tool: &str, args: &[String]| -> Result<(), SigningError> {
                let out = Command::new(tool).args(args).output().map_err(|source| {
                    SigningError::Spawn {
                        tool: tool.to_owned(),
                        source,
                    }
                })?;
                if out.status.success() {
                    Ok(())
                } else {
                    Err(SigningError::CommandFailed {
                        tool: tool.to_owned(),
                        status: out.status.to_string(),
                        stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
                    })
                }
            };

        // Device development path (free Personal Team): sign inner dylibs first,
        // then the bundle — NO `--deep`, NO `--options runtime` (both wrong for a
        // visionOS device bundle), NO notarization. The profile is already
        // embedded in the `.app` by the packager.
        if development {
            let frameworks = self.bundle.join("Frameworks");
            if let Ok(entries) = std::fs::read_dir(&frameworks) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "dylib") {
                        run(
                            "codesign",
                            &[
                                "--force".to_owned(),
                                "--sign".to_owned(),
                                identity.to_owned(),
                                path.display().to_string(),
                            ],
                        )?;
                    }
                }
            }
            let mut cs = vec!["--force".to_owned()];
            if let Some(ents) = entitlements {
                cs.push("--entitlements".to_owned());
                cs.push(ents.display().to_string());
            }
            cs.push("--sign".to_owned());
            cs.push(identity.to_owned());
            cs.push(bundle.clone());
            return run("codesign", &cs);
        }

        // Store / notarized path (macOS-identical): deep, hardened runtime,
        // optional entitlements + notarization.
        let mut cs = vec![
            "--deep".to_owned(),
            "--force".to_owned(),
            "--options".to_owned(),
            "runtime".to_owned(),
        ];
        if let Some(ents) = entitlements {
            cs.push("--entitlements".to_owned());
            cs.push(ents.display().to_string());
        }
        cs.push("--sign".to_owned());
        cs.push(identity.to_owned());
        cs.push(bundle.clone());
        run("codesign", &cs)?;

        // Notarize + staple when a keychain profile is configured.
        if let Some(profile) = notary {
            run(
                "xcrun",
                &[
                    "notarytool".to_owned(),
                    "submit".to_owned(),
                    bundle.clone(),
                    "--keychain-profile".to_owned(),
                    profile.to_owned(),
                    "--wait".to_owned(),
                ],
            )?;
            run(
                "xcrun",
                &["stapler".to_owned(), "staple".to_owned(), bundle.clone()],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn none_is_unsigned_noop() {
        let cfg = SigningConfig::None;
        assert!(!cfg.is_signed());
        assert!(!cfg.is_notarized());
        let plan = SigningPlan::new(&cfg, Path::new("out/App"));
        assert!(plan.execute().is_ok());
        assert!(plan.describe().contains("unsigned"));
    }

    #[test]
    fn macos_describe_includes_notary_when_profiled() {
        let cfg = SigningConfig::MacOs {
            identity: "Developer ID Application: Acme (TEAMID)".to_string(),
            notary_profile: Some("acme-notary".to_string()),
        };
        assert!(cfg.is_signed());
        assert!(cfg.is_notarized());
        let plan = SigningPlan::new(&cfg, Path::new("out/App.app"));
        let d = plan.describe();
        assert!(d.contains("codesign"));
        assert!(d.contains("notarytool"));
        assert!(d.contains("stapler"));
        // execute() now runs for real on macOS: a bogus identity → CommandFailed;
        // on a non-macOS host → NotImplemented. Either way it must be an error.
        assert!(plan.execute().is_err());
    }

    #[test]
    fn visionos_describe_includes_entitlements_and_notary() {
        let cfg = SigningConfig::VisionOs {
            identity: "Apple Development: Acme (TEAMID)".to_string(),
            entitlements: Some(PathBuf::from("idtx.entitlements")),
            notary_profile: Some("acme-notary".to_string()),
            provisioning_profile: None,
            development: false,
        };
        assert!(cfg.is_signed());
        assert!(cfg.is_notarized());
        assert!(!cfg.is_visionos_development());
        let d = SigningPlan::new(&cfg, Path::new("out/Idtx.app")).describe();
        assert!(d.contains("codesign"));
        assert!(d.contains("--entitlements \"idtx.entitlements\""));
        assert!(d.contains("--options runtime"));
        assert!(d.contains("notarytool"));
        assert!(d.contains("stapler"));
    }

    #[test]
    fn visionos_device_development_describe_is_free_team_safe() {
        let cfg = SigningConfig::VisionOs {
            identity: "Apple Development: Jane Doe (TEAMID1234)".to_string(),
            entitlements: Some(PathBuf::from("dev.entitlements")),
            notary_profile: None,
            provisioning_profile: Some(PathBuf::from("dev.mobileprovision")),
            development: true,
        };
        assert!(cfg.is_signed());
        assert!(!cfg.is_notarized());
        assert!(cfg.is_visionos_development());
        let d = SigningPlan::new(&cfg, Path::new("out/Idtx.app")).describe();
        // Inner dylibs are signed before the bundle.
        assert!(d.contains("Frameworks/*.dylib"));
        assert!(d.contains("--entitlements \"dev.entitlements\""));
        // The provisioning profile is embedded.
        assert!(d.contains("embedded.mobileprovision"));
        // Crucially NOT the macOS hardened-runtime / notarization path.
        assert!(
            !d.contains("--options runtime"),
            "device bundle must not use hardened runtime"
        );
        assert!(
            !d.contains("--deep"),
            "inner dylibs are signed individually, not --deep"
        );
        assert!(
            !d.contains("notarytool"),
            "device dev builds are never notarized"
        );
    }

    #[test]
    fn visionos_adhoc_without_notary_is_signed_not_notarized() {
        let cfg = SigningConfig::VisionOs {
            identity: "-".to_string(),
            entitlements: None,
            notary_profile: None,
            provisioning_profile: None,
            development: false,
        };
        assert!(cfg.is_signed());
        assert!(!cfg.is_notarized());
        let d = SigningPlan::new(&cfg, Path::new("x")).describe();
        assert!(!d.contains("notarytool"));
    }

    /// On macOS, `execute()` really runs `codesign` — an ad-hoc (`-`) signature
    /// over a throwaway `.app` directory must succeed, proving the live signing
    /// path (not just `describe`). Skipped off macOS (no `codesign`).
    #[cfg(target_os = "macos")]
    #[test]
    fn visionos_adhoc_execute_signs_a_bundle() {
        let dir = std::env::temp_dir().join("idtx-signing-test.app");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Info.plist"), "<plist></plist>").unwrap();

        let cfg = SigningConfig::VisionOs {
            identity: "-".to_string(),
            entitlements: None,
            notary_profile: None,
            provisioning_profile: None,
            development: false,
        };
        let res = SigningPlan::new(&cfg, &dir).execute();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(res.is_ok(), "ad-hoc codesign should succeed: {res:?}");
    }

    #[test]
    fn macos_without_profile_is_signed_not_notarized() {
        let cfg = SigningConfig::MacOs {
            identity: "id".to_string(),
            notary_profile: None,
        };
        assert!(cfg.is_signed());
        assert!(!cfg.is_notarized());
        assert!(
            !SigningPlan::new(&cfg, Path::new("x"))
                .describe()
                .contains("notarytool")
        );
    }

    #[test]
    fn windows_describe_carries_timestamp() {
        let cfg = SigningConfig::Windows {
            certificate: "ABCD1234".to_string(),
            timestamp_url: Some("http://timestamp.digicert.com".to_string()),
        };
        let d = SigningPlan::new(&cfg, Path::new("out/app.exe")).describe();
        assert!(d.contains("signtool"));
        assert!(d.contains("timestamp.digicert.com"));
    }

    #[test]
    fn android_describe_signs_and_verifies_apk() {
        let cfg = SigningConfig::Android {
            keystore: "release.jks".to_string(),
            key_alias: "idtx".to_string(),
        };
        assert!(cfg.is_signed());
        assert!(!cfg.is_notarized());
        let plan = SigningPlan::new(&cfg, Path::new("out/app-release.apk"));
        let d = plan.describe();
        assert!(d.contains("apksigner sign"));
        assert!(d.contains("--ks \"release.jks\""));
        assert!(d.contains("--ks-key-alias \"idtx\""));
        assert!(d.contains("apksigner verify"));
        assert!(matches!(
            plan.execute(),
            Err(SigningError::NotImplemented(_))
        ));
    }
}
