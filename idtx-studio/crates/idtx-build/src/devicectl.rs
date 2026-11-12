//! `xcrun devicectl` wrapper — list paired Apple devices and install/launch a
//! signed `.app` on a physical Vision Pro, for development testing.
//!
//! `devicectl` is the Xcode-26-era replacement for `ios-deploy` (which has no
//! visionOS support). It does **not** sign — the `.app` must already be signed
//! with a development identity and embed a provisioning profile whose
//! provisioned-devices list contains the target device's UDID (see
//! [`crate::apple_identity`] / [`crate::visionos_assemble`]).
//!
//! The arg-vector builders and the `--json-output` parser are pure and
//! unit-tested; [`list_paired_devices`] / [`install_app`] / [`launch_app`] shell
//! out (macOS only).

#![allow(clippy::doc_markdown)]

use std::path::Path;

/// Errors from `devicectl`.
#[derive(Debug, thiserror::Error)]
pub enum DeviceCtlError {
    /// `devicectl` is only available on macOS.
    #[error("`xcrun devicectl` is only available on macOS")]
    NotMacOs,
    /// `xcrun` could not be spawned.
    #[error("failed to spawn `xcrun devicectl`: {0}")]
    Spawn(std::io::Error),
    /// A `devicectl` invocation exited non-zero.
    #[error("`xcrun {argv}` failed ({status}): {stderr}")]
    CommandFailed {
        /// The devicectl args.
        argv: String,
        /// Exit status.
        status: String,
        /// Trimmed stderr.
        stderr: String,
    },
    /// The `--json-output` could not be parsed.
    #[error("could not parse `devicectl` JSON output: {0}")]
    Json(String),
    /// No paired visionOS device was found.
    #[error("no paired Apple Vision Pro found (is it paired in Xcode and in Developer Mode?)")]
    NoVisionOsDevice,
}

/// A paired device as reported by `devicectl list devices`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// The identifier passed to `--device` (the coredevice identifier/UDID).
    pub identifier: String,
    /// Human-readable device name.
    pub name: String,
    /// Platform string (e.g. `xrOS` / `visionOS`).
    pub platform: String,
    /// `connectionProperties.tunnelState` — `connected`, `disconnected`,
    /// `unavailable`, or empty when the schema omits it. Drives reachability
    /// ranking so install targets a device `devicectl` can actually reach.
    pub tunnel_state: String,
}

impl DeviceInfo {
    /// Whether this is a visionOS / Vision Pro device.
    #[must_use]
    pub fn is_visionos(&self) -> bool {
        let p = self.platform.to_ascii_lowercase();
        p.contains("xr") || p.contains("vision")
    }

    /// How reachable `devicectl` considers this device, for selection ranking:
    /// `2` currently connected, `1` reachable on demand (e.g. `disconnected`
    /// over localNetwork, or an unknown/omitted state — tried optimistically),
    /// `0` `unavailable` (no transport — install would fail with CoreDevice
    /// 1011 "unable to locate a device"). Higher wins.
    #[must_use]
    pub fn reachability(&self) -> u8 {
        match self.tunnel_state.to_ascii_lowercase().as_str() {
            "connected" => 2,
            "unavailable" => 0,
            // `disconnected` (devicectl brings the tunnel up on demand) or an
            // empty/unknown state on an older schema: try it rather than skip.
            _ => 1,
        }
    }
}

/// `devicectl list devices --json-output <path>` args.
#[must_use]
pub fn list_devices_args(json_out: &Path) -> Vec<String> {
    vec![
        "devicectl".to_owned(),
        "list".to_owned(),
        "devices".to_owned(),
        "--json-output".to_owned(),
        json_out.display().to_string(),
    ]
}

/// `devicectl device install app --device <id> <app>` args.
#[must_use]
pub fn install_app_args(device: &str, app: &Path) -> Vec<String> {
    vec![
        "devicectl".to_owned(),
        "device".to_owned(),
        "install".to_owned(),
        "app".to_owned(),
        "--device".to_owned(),
        device.to_owned(),
        app.display().to_string(),
    ]
}

/// `devicectl device process launch --device <id> --terminate-existing
/// <bundle-id>` args. Launched detached (no `--console`) because an immersive
/// app runs its render loop indefinitely and never exits.
#[must_use]
pub fn launch_args(device: &str, bundle_id: &str) -> Vec<String> {
    vec![
        "devicectl".to_owned(),
        "device".to_owned(),
        "process".to_owned(),
        "launch".to_owned(),
        "--device".to_owned(),
        device.to_owned(),
        "--terminate-existing".to_owned(),
        bundle_id.to_owned(),
    ]
}

/// `devicectl device copy from --device <id> --domain-type appDataContainer
/// --domain-identifier <bundle-id> --source <rel> --destination <local>` args.
/// Copies a file (or directory) out of the app's data container (the `tmp/`
/// subtree is where the runtime writes `idtx_visionos.log` and the
/// `idtx-xr-metrics-*.json` dumps). `source` is relative to the container root
/// (e.g. `tmp` or `tmp/idtx-xr-metrics-datacenter-123.json`). Matches the proven
/// log-pull invocation — no `--user` (visionOS rejects it with a
/// "No provider was found" container-lookup error).
#[must_use]
pub fn copy_from_args(device: &str, bundle_id: &str, source: &str, dest: &Path) -> Vec<String> {
    vec![
        "devicectl".to_owned(),
        "device".to_owned(),
        "copy".to_owned(),
        "from".to_owned(),
        "--device".to_owned(),
        device.to_owned(),
        "--domain-type".to_owned(),
        "appDataContainer".to_owned(),
        "--domain-identifier".to_owned(),
        bundle_id.to_owned(),
        "--source".to_owned(),
        source.to_owned(),
        "--destination".to_owned(),
        dest.display().to_string(),
    ]
}

/// Parse `devicectl list devices --json-output` into [`DeviceInfo`]s. Tolerant of
/// schema drift: reads `result.devices[]`, taking the identifier from
/// `identifier` (or `hardwareProperties.udid`), the name from
/// `deviceProperties.name`, and the platform from `hardwareProperties.platform`.
///
/// # Errors
/// [`DeviceCtlError::Json`] if the top-level JSON does not parse.
pub fn parse_devices_json(json: &str) -> Result<Vec<DeviceInfo>, DeviceCtlError> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| DeviceCtlError::Json(e.to_string()))?;
    let devices = root
        .get("result")
        .and_then(|r| r.get("devices"))
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for dev in devices {
        let hw = dev.get("hardwareProperties");
        let identifier = dev
            .get("identifier")
            .and_then(|v| v.as_str())
            .or_else(|| hw.and_then(|h| h.get("udid")).and_then(|v| v.as_str()))
            .unwrap_or_default()
            .to_owned();
        let name = dev
            .get("deviceProperties")
            .and_then(|p| p.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        let platform = hw
            .and_then(|h| h.get("platform"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        let tunnel_state = dev
            .get("connectionProperties")
            .and_then(|c| c.get("tunnelState"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        if identifier.is_empty() {
            continue;
        }
        out.push(DeviceInfo {
            identifier,
            name,
            platform,
            tunnel_state,
        });
    }
    Ok(out)
}

/// Pick the most-reachable visionOS device from a device list.
///
/// `devicectl list devices` returns *every* paired device, including stale
/// pairings the host cannot currently reach. Picking the first visionOS entry
/// (the old behavior) can therefore target an `unavailable` device and fail with
/// CoreDevice error 1011. Rank by [`DeviceInfo::reachability`] instead so a
/// connected / on-demand-reachable headset wins over an unreachable pairing.
/// Ties keep the earliest device (stable `fold`, upgrades only on a strictly
/// higher score).
#[must_use]
pub fn select_visionos_device(devices: &[DeviceInfo]) -> Option<&DeviceInfo> {
    devices
        .iter()
        .filter(|d| d.is_visionos())
        .fold(None, |best: Option<&DeviceInfo>, d| match best {
            Some(b) if b.reachability() >= d.reachability() => Some(b),
            _ => Some(d),
        })
}

// ── macOS shell-outs ─────────────────────────────────────────────────────────

/// List paired devices via `devicectl list devices`.
///
/// # Errors
/// [`DeviceCtlError`] if `xcrun` fails or the JSON cannot be read (or off macOS).
#[cfg(target_os = "macos")]
pub fn list_paired_devices() -> Result<Vec<DeviceInfo>, DeviceCtlError> {
    let tmp = std::env::temp_dir().join("idtx-devicectl-devices.json");
    run_devicectl(&list_devices_args(&tmp))?;
    let json = std::fs::read_to_string(&tmp).map_err(|e| DeviceCtlError::Json(e.to_string()))?;
    let _ = std::fs::remove_file(&tmp);
    parse_devices_json(&json)
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn list_paired_devices() -> Result<Vec<DeviceInfo>, DeviceCtlError> {
    Err(DeviceCtlError::NotMacOs)
}

/// Install a signed `.app` on `device`.
///
/// # Errors
/// [`DeviceCtlError`] if the install fails (or off macOS).
#[cfg(target_os = "macos")]
pub fn install_app(device: &str, app: &Path) -> Result<(), DeviceCtlError> {
    run_devicectl(&install_app_args(device, app))
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn install_app(_device: &str, _app: &Path) -> Result<(), DeviceCtlError> {
    Err(DeviceCtlError::NotMacOs)
}

/// Launch `bundle_id` on `device` (detached).
///
/// # Errors
/// [`DeviceCtlError`] if the launch fails (or off macOS).
#[cfg(target_os = "macos")]
pub fn launch_app(device: &str, bundle_id: &str) -> Result<(), DeviceCtlError> {
    run_devicectl(&launch_args(device, bundle_id))
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn launch_app(_device: &str, _bundle_id: &str) -> Result<(), DeviceCtlError> {
    Err(DeviceCtlError::NotMacOs)
}

/// Copy a file out of `bundle_id`'s data container on `device` to `dest`.
/// `source` is relative to the container root (e.g. `tmp/idtx-xr-metrics-*.json`).
///
/// # Errors
/// [`DeviceCtlError`] if the copy fails (or off macOS).
#[cfg(target_os = "macos")]
pub fn copy_from(
    device: &str,
    bundle_id: &str,
    source: &str,
    dest: &Path,
) -> Result<(), DeviceCtlError> {
    run_devicectl(&copy_from_args(device, bundle_id, source, dest))
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn copy_from(
    _device: &str,
    _bundle_id: &str,
    _source: &str,
    _dest: &Path,
) -> Result<(), DeviceCtlError> {
    Err(DeviceCtlError::NotMacOs)
}

#[cfg(target_os = "macos")]
fn run_devicectl(args: &[String]) -> Result<(), DeviceCtlError> {
    let out = std::process::Command::new("xcrun")
        .args(args)
        .output()
        .map_err(DeviceCtlError::Spawn)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(DeviceCtlError::CommandFailed {
            argv: args.join(" "),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arg_vectors() {
        assert_eq!(
            list_devices_args(Path::new("/tmp/d.json")),
            vec![
                "devicectl",
                "list",
                "devices",
                "--json-output",
                "/tmp/d.json"
            ]
        );
        assert_eq!(
            install_app_args("UDID-1", Path::new("/x/App.app")),
            vec![
                "devicectl",
                "device",
                "install",
                "app",
                "--device",
                "UDID-1",
                "/x/App.app"
            ]
        );
        let launch = launch_args("UDID-1", "com.sap.apeirora.idtx.app");
        assert_eq!(&launch[0..4], &["devicectl", "device", "process", "launch"]);
        assert!(launch.contains(&"--terminate-existing".to_owned()));
        assert!(launch.contains(&"com.sap.apeirora.idtx.app".to_owned()));
        // Detached: no console attach (immersive app never exits).
        assert!(!launch.iter().any(|a| a.contains("console")));

        let copy = copy_from_args(
            "UDID-1",
            "com.sap.apeirora.idtx.vision",
            "tmp/idtx-xr-metrics-datacenter-123.json",
            Path::new("/local/out.json"),
        );
        assert_eq!(&copy[0..4], &["devicectl", "device", "copy", "from"]);
        // Pulls from the app's data container (the `tmp/` sink the runtime writes).
        assert!(copy.contains(&"appDataContainer".to_owned()));
        assert!(copy.contains(&"com.sap.apeirora.idtx.vision".to_owned()));
        assert!(copy.contains(&"tmp/idtx-xr-metrics-datacenter-123.json".to_owned()));
        assert!(copy.contains(&"/local/out.json".to_owned()));
        assert!(copy.contains(&"--device".to_owned()));
    }

    #[test]
    fn parse_and_select_visionos_device() {
        let json = r#"{
            "result": {
                "devices": [
                    {
                        "identifier": "AAAA-IPHONE",
                        "deviceProperties": { "name": "Jane's iPhone" },
                        "hardwareProperties": { "platform": "iOS", "udid": "AAAA" }
                    },
                    {
                        "identifier": "BBBB-VISION",
                        "deviceProperties": { "name": "Jane's Vision Pro" },
                        "hardwareProperties": { "platform": "xrOS", "udid": "BBBB" },
                        "connectionProperties": { "tunnelState": "connected" }
                    }
                ]
            }
        }"#;
        let devices = parse_devices_json(json).expect("parse");
        assert_eq!(devices.len(), 2);
        let vp = select_visionos_device(&devices).expect("vision pro");
        assert_eq!(vp.identifier, "BBBB-VISION");
        assert_eq!(vp.name, "Jane's Vision Pro");
        assert!(vp.is_visionos());
        assert!(!devices[0].is_visionos());
    }

    #[test]
    fn select_prefers_reachable_visionos_over_unavailable_pairing() {
        // Two paired Vision Pros: the first is `unavailable` (no transport — the
        // field-reported bug), the second `disconnected` over localNetwork and
        // reachable on demand. The reachable one must win despite coming later.
        let json = r#"{
            "result": {
                "devices": [
                    {
                        "identifier": "UNAVAILABLE-AVP",
                        "deviceProperties": { "name": "Apple Vision Pro" },
                        "hardwareProperties": { "platform": "visionOS", "udid": "X1" },
                        "connectionProperties": { "tunnelState": "unavailable" }
                    },
                    {
                        "identifier": "LIVE-AVP",
                        "deviceProperties": { "name": "SAP visionOS Demo" },
                        "hardwareProperties": { "platform": "visionOS", "udid": "X2" },
                        "connectionProperties": { "tunnelState": "disconnected" }
                    }
                ]
            }
        }"#;
        let devices = parse_devices_json(json).expect("parse");
        assert_eq!(devices[0].reachability(), 0);
        assert_eq!(devices[1].reachability(), 1);
        let vp = select_visionos_device(&devices).expect("vision pro");
        assert_eq!(vp.identifier, "LIVE-AVP");
    }

    #[test]
    fn parse_falls_back_to_udid_and_tolerates_empty() {
        let json = r#"{"result":{"devices":[
            {"hardwareProperties":{"platform":"visionOS","udid":"CCCC"}}
        ]}}"#;
        let devices = parse_devices_json(json).unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].identifier, "CCCC");
        // An omitted tunnelState → empty → optimistic reachability 1 (still a
        // selectable candidate, so older schemas don't regress to "no device").
        assert_eq!(devices[0].tunnel_state, "");
        assert_eq!(devices[0].reachability(), 1);
        assert!(select_visionos_device(&devices).is_some());
        // No devices key at all → empty, not an error.
        assert!(parse_devices_json("{}").unwrap().is_empty());
        assert!(parse_devices_json("not json").is_err());
    }
}
