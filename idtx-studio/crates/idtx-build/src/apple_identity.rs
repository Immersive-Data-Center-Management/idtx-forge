//! Auto-discovery of the Apple **development** signing identity and provisioning
//! profile used to sign a visionOS device `.app` with a **free "Personal Team"**
//! (no paid Apple Developer Program).
//!
//! A free Apple ID added to Xcode mints an `Apple Development: NAME (TEAMID)`
//! certificate into the login keychain and a development provisioning profile
//! (under `~/Library/MobileDevice/Provisioning Profiles/*.mobileprovision`) tied
//! to the registered device UDID. That one-time mint goes through Xcode /
//! `xcodebuild -allowProvisioningUpdates` (a free team has no web portal); this
//! module only **consumes** the results: it lists codesigning identities via
//! `security find-identity` and reads the CMS-wrapped profile plist via
//! `security cms -D`.
//!
//! The parsers ([`parse_identities`], [`parse_mobileprovision_plist`],
//! [`select_profile`], [`MobileProvision::resolved_bundle_id`]) are pure and
//! unit-tested; only [`discover_development_identity`] /
//! [`discover_profile_for`] shell out (macOS only).

#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};

/// Errors from identity / provisioning-profile discovery.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// Discovery is macOS-only and this host is not macOS.
    #[error("Apple signing-identity discovery is only available on macOS")]
    NotMacOs,
    /// A `security` invocation could not be spawned.
    #[error("failed to spawn `security`: {0}")]
    Spawn(std::io::Error),
    /// A `security` invocation exited non-zero.
    #[error("`security {argv}` failed ({status}): {stderr}")]
    CommandFailed {
        /// The args passed to `security`.
        argv: String,
        /// Exit status.
        status: String,
        /// Trimmed stderr.
        stderr: String,
    },
    /// No `Apple Development:` identity was found in the login keychain.
    #[error(
        "no `Apple Development` codesigning identity found in the login keychain.\n\
         Add your (free) Apple ID in Xcode > Settings > Accounts and let it create the\n\
         `Apple Development` certificate (one-time), then retry."
    )]
    NoDevelopmentIdentity,
    /// No provisioning profile matched the requested bundle id.
    #[error(
        "no development provisioning profile found for bundle id `{bundle_id}` in {dir}.\n\
         Build once in Xcode (or `xcodebuild -allowProvisioningUpdates`) with your\n\
         Personal Team so a profile for this app + device is generated, then retry."
    )]
    NoProfile {
        /// The bundle id searched for.
        bundle_id: String,
        /// The directory scanned.
        dir: PathBuf,
    },
}

/// A parsed Apple provisioning profile (the plist inside the CMS envelope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MobileProvision {
    /// The signing team id (`TeamIdentifier` first entry / the prefix of
    /// `application-identifier`).
    pub team_id: String,
    /// The full `application-identifier` entitlement, e.g.
    /// `TEAMID.com.sap.apeirora.idtx.myapp` or the wildcard `TEAMID.*`.
    pub app_id: String,
    /// The bundle-id portion of [`Self::app_id`] (the `TEAMID.` prefix stripped);
    /// `*` for a team wildcard profile.
    pub bundle_id: String,
    /// UDIDs this development profile is provisioned for.
    pub provisioned_devices: Vec<String>,
    /// ISO-8601 `ExpirationDate`, if present (string compare is chronological).
    pub expiration: Option<String>,
}

impl MobileProvision {
    /// Whether this is a team wildcard profile (`bundle_id == "*"`), which can
    /// sign any bundle id under the team.
    #[must_use]
    pub fn is_wildcard(&self) -> bool {
        self.bundle_id == "*" || self.bundle_id.ends_with(".*")
    }

    /// Whether this profile can sign `bundle_id`: an exact match, or a wildcard
    /// (`*`, or a `prefix.*` whose prefix matches).
    #[must_use]
    pub fn matches_bundle_id(&self, bundle_id: &str) -> bool {
        if self.bundle_id == bundle_id {
            return true;
        }
        if self.bundle_id == "*" {
            return true;
        }
        if let Some(prefix) = self.bundle_id.strip_suffix(".*") {
            return bundle_id == prefix || bundle_id.starts_with(&format!("{prefix}."));
        }
        false
    }

    /// Reconcile a `configured` bundle id against this profile: a non-wildcard
    /// profile pins the Info.plist `CFBundleIdentifier` to its own id (an install
    /// is rejected on mismatch); a wildcard profile keeps the configured id.
    #[must_use]
    pub fn resolved_bundle_id(&self, configured: &str) -> String {
        if self.is_wildcard() {
            configured.to_owned()
        } else {
            self.bundle_id.clone()
        }
    }
}

/// Parse `security find-identity -v -p codesigning` output into the quoted
/// identity names (e.g. `Apple Development: Jane Doe (TEAMID1234)`).
///
/// Each identity line looks like: `  1) <40-hex-sha1> "<name>"`.
#[must_use]
pub fn parse_identities(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| {
            let start = line.find('"')?;
            let rest = &line[start + 1..];
            let end = rest.find('"')?;
            Some(rest[..end].to_owned())
        })
        .collect()
}

/// Pick the first `Apple Development` identity from parsed identity names.
#[must_use]
pub fn pick_development_identity(identities: &[String]) -> Option<String> {
    identities
        .iter()
        .find(|n| n.contains("Apple Development:"))
        .cloned()
}

/// Parse a provisioning-profile plist (the XML the CMS envelope wraps) into a
/// [`MobileProvision`]. Hand-rolled key extraction (consistent with the repo's
/// other hand-written plist handling) — no `plist` crate dependency.
#[must_use]
pub fn parse_mobileprovision_plist(xml: &str) -> Option<MobileProvision> {
    let app_id = first_string_after_key(xml, "application-identifier")?;
    // Team id: the `application-identifier` prefix, or `TeamIdentifier[0]`.
    let team_id = app_id
        .split_once('.')
        .map(|(t, _)| t.to_owned())
        .or_else(|| {
            array_strings_after_key(xml, "TeamIdentifier")
                .into_iter()
                .next()
        })
        .unwrap_or_default();
    // Bundle id: strip the `TEAMID.` prefix off the application-identifier.
    let bundle_id = app_id
        .strip_prefix(&format!("{team_id}."))
        .unwrap_or(&app_id)
        .to_owned();
    Some(MobileProvision {
        team_id,
        app_id,
        bundle_id,
        provisioned_devices: array_strings_after_key(xml, "ProvisionedDevices"),
        expiration: date_after_key(xml, "ExpirationDate"),
    })
}

/// From `(path, profile)` candidates, pick the newest-expiring profile that can
/// sign `bundle_id`. Exact-match profiles win over wildcards; among equal
/// specificity, the latest `ExpirationDate` wins.
#[must_use]
pub fn select_profile(
    candidates: &[(PathBuf, MobileProvision)],
    bundle_id: &str,
) -> Option<PathBuf> {
    candidates
        .iter()
        .filter(|(_, p)| p.matches_bundle_id(bundle_id))
        .max_by(|(_, a), (_, b)| {
            // Prefer exact (non-wildcard) matches, then latest expiry.
            let a_exact = !a.is_wildcard();
            let b_exact = !b.is_wildcard();
            a_exact
                .cmp(&b_exact)
                .then_with(|| a.expiration.cmp(&b.expiration))
        })
        .map(|(path, _)| path.clone())
}

/// The directories Xcode writes development provisioning profiles to, newest
/// convention first: the Xcode-16+ location
/// (`~/Library/Developer/Xcode/UserData/Provisioning Profiles`) then the classic
/// `~/Library/MobileDevice/Provisioning Profiles`.
#[must_use]
pub fn profile_search_dirs() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    vec![
        home.join("Library/Developer/Xcode/UserData/Provisioning Profiles"),
        home.join("Library/MobileDevice/Provisioning Profiles"),
    ]
}

/// The classic development provisioning-profiles directory (`~/Library/
/// MobileDevice/Provisioning Profiles`). See [`profile_search_dirs`] for the full
/// set of locations scanned.
#[must_use]
pub fn default_profiles_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library")
            .join("MobileDevice")
            .join("Provisioning Profiles")
    })
}

// ── macOS shell-outs ─────────────────────────────────────────────────────────

/// Discover the first `Apple Development` identity in the login keychain via
/// `security find-identity -v -p codesigning`.
///
/// # Errors
/// [`IdentityError`] if `security` is unavailable/fails or no development
/// identity exists (or off macOS).
#[cfg(target_os = "macos")]
pub fn discover_development_identity() -> Result<String, IdentityError> {
    let stdout = run_security(&["find-identity", "-v", "-p", "codesigning"])?;
    pick_development_identity(&parse_identities(&stdout))
        .ok_or(IdentityError::NoDevelopmentIdentity)
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn discover_development_identity() -> Result<String, IdentityError> {
    Err(IdentityError::NotMacOs)
}

/// Decode a single `.mobileprovision` file (CMS → plist) via
/// `security cms -D -i <file>` and parse it.
///
/// # Errors
/// [`IdentityError`] if `security` fails or the plist cannot be parsed (or off
/// macOS).
#[cfg(target_os = "macos")]
pub fn read_profile(path: &Path) -> Result<MobileProvision, IdentityError> {
    let xml = run_security(&["cms", "-D", "-i", &path.display().to_string()])?;
    parse_mobileprovision_plist(&xml).ok_or_else(|| IdentityError::CommandFailed {
        argv: format!("cms -D -i {}", path.display()),
        status: "parse".to_owned(),
        stderr: "profile plist missing application-identifier".to_owned(),
    })
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn read_profile(_path: &Path) -> Result<MobileProvision, IdentityError> {
    Err(IdentityError::NotMacOs)
}

/// Scan the provisioning-profiles directory and pick the newest profile matching
/// `bundle_id`.
///
/// # Errors
/// [`IdentityError::NoProfile`] when none match (or off macOS / HOME unset).
#[cfg(target_os = "macos")]
pub fn discover_profile_for(bundle_id: &str) -> Result<PathBuf, IdentityError> {
    let dirs = profile_search_dirs();
    if dirs.is_empty() {
        return Err(IdentityError::NoProfile {
            bundle_id: bundle_id.to_owned(),
            dir: PathBuf::from("$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles"),
        });
    }
    let mut candidates = Vec::new();
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "mobileprovision")
                && let Ok(profile) = read_profile(&path) {
                    candidates.push((path, profile));
                }
        }
    }
    select_profile(&candidates, bundle_id).ok_or_else(|| IdentityError::NoProfile {
        bundle_id: bundle_id.to_owned(),
        dir: dirs.into_iter().next().unwrap_or_default(),
    })
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn discover_profile_for(bundle_id: &str) -> Result<PathBuf, IdentityError> {
    let _ = bundle_id;
    Err(IdentityError::NotMacOs)
}

#[cfg(target_os = "macos")]
fn run_security(args: &[&str]) -> Result<String, IdentityError> {
    let out = std::process::Command::new("security")
        .args(args)
        .output()
        .map_err(IdentityError::Spawn)?;
    if !out.status.success() {
        return Err(IdentityError::CommandFailed {
            argv: args.join(" "),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ── pure plist key extraction ─────────────────────────────────────────────────

/// The `<string>…</string>` immediately following `<key>KEY</key>`.
fn first_string_after_key(xml: &str, key: &str) -> Option<String> {
    let after = slice_after_key(xml, key)?;
    extract_tag(after, "string")
}

/// All `<string>…</string>` inside the `<array>…</array>` that follows
/// `<key>KEY</key>`.
fn array_strings_after_key(xml: &str, key: &str) -> Vec<String> {
    let Some(after) = slice_after_key(xml, key) else {
        return Vec::new();
    };
    let Some(arr_start) = after.find("<array>") else {
        return Vec::new();
    };
    let rest = &after[arr_start + "<array>".len()..];
    let Some(arr_end) = rest.find("</array>") else {
        return Vec::new();
    };
    let body = &rest[..arr_end];
    let mut out = Vec::new();
    let mut cursor = body;
    while let Some(open) = cursor.find("<string>") {
        let val_start = open + "<string>".len();
        let Some(rel_close) = cursor[val_start..].find("</string>") else {
            break;
        };
        let close = val_start + rel_close;
        out.push(cursor[val_start..close].to_owned());
        cursor = &cursor[close + "</string>".len()..];
    }
    out
}

/// The `<date>…</date>` immediately following `<key>KEY</key>`.
fn date_after_key(xml: &str, key: &str) -> Option<String> {
    let after = slice_after_key(xml, key)?;
    extract_tag(after, "date")
}

/// The substring just after `<key>KEY</key>`.
fn slice_after_key<'a>(xml: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("<key>{key}</key>");
    let idx = xml.find(&needle)?;
    Some(&xml[idx + needle.len()..])
}

/// The text content of the first `<tag>…</tag>` in `s`.
fn extract_tag(s: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&close)? + start;
    Some(s[start..end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>AppIDName</key><string>IDTX Vision</string>
    <key>TeamIdentifier</key>
    <array>
        <string>ABCDE12345</string>
    </array>
    <key>ProvisionedDevices</key>
    <array>
        <string>00008112-000A1B2C3D4E</string>
        <string>00008112-FFEEDDCCBBAA</string>
    </array>
    <key>Entitlements</key>
    <dict>
        <key>application-identifier</key><string>ABCDE12345.com.sap.apeirora.idtx.vision</string>
        <key>get-task-allow</key><true/>
        <key>com.apple.developer.team-identifier</key><string>ABCDE12345</string>
    </dict>
    <key>ExpirationDate</key><date>2026-10-15T12:00:00Z</date>
</dict>
</plist>
"#;

    #[test]
    fn parses_profile_fields() {
        let p = parse_mobileprovision_plist(FIXTURE).expect("parse");
        assert_eq!(p.team_id, "ABCDE12345");
        assert_eq!(p.app_id, "ABCDE12345.com.sap.apeirora.idtx.vision");
        assert_eq!(p.bundle_id, "com.sap.apeirora.idtx.vision");
        assert_eq!(p.provisioned_devices.len(), 2);
        assert!(
            p.provisioned_devices
                .contains(&"00008112-FFEEDDCCBBAA".to_owned())
        );
        assert_eq!(p.expiration.as_deref(), Some("2026-10-15T12:00:00Z"));
        assert!(!p.is_wildcard());
    }

    #[test]
    fn parses_identities_quoted_names() {
        let out = "  1) AAAA1111 \"Apple Development: Jane Doe (TEAMID1234)\"\n  \
                   2) BBBB2222 \"Apple Distribution: Acme (TEAMID1234)\"\n     \
                   2 valid identities found";
        let ids = parse_identities(out);
        assert_eq!(ids.len(), 2);
        let dev = pick_development_identity(&ids).expect("dev identity");
        assert_eq!(dev, "Apple Development: Jane Doe (TEAMID1234)");
    }

    #[test]
    fn wildcard_matching_and_resolution() {
        let wildcard = MobileProvision {
            team_id: "T".into(),
            app_id: "T.*".into(),
            bundle_id: "*".into(),
            provisioned_devices: vec![],
            expiration: Some("2026-01-01T00:00:00Z".into()),
        };
        assert!(wildcard.is_wildcard());
        assert!(wildcard.matches_bundle_id("anything.at.all"));
        // A wildcard keeps the configured id; an exact profile pins its own.
        assert_eq!(
            wildcard.resolved_bundle_id("com.sap.apeirora.idtx.foo"),
            "com.sap.apeirora.idtx.foo"
        );

        let exact = parse_mobileprovision_plist(FIXTURE).unwrap();
        assert!(exact.matches_bundle_id("com.sap.apeirora.idtx.vision"));
        assert!(!exact.matches_bundle_id("com.sap.apeirora.idtx.other"));
        assert_eq!(
            exact.resolved_bundle_id("ignored"),
            "com.sap.apeirora.idtx.vision"
        );
    }

    #[test]
    fn select_prefers_exact_then_newest() {
        let exact = parse_mobileprovision_plist(FIXTURE).unwrap();
        let wildcard = MobileProvision {
            team_id: "ABCDE12345".into(),
            app_id: "ABCDE12345.*".into(),
            bundle_id: "*".into(),
            provisioned_devices: vec![],
            expiration: Some("2027-01-01T00:00:00Z".into()), // newer, but wildcard
        };
        let candidates = vec![
            (PathBuf::from("/wild.mobileprovision"), wildcard),
            (PathBuf::from("/exact.mobileprovision"), exact),
        ];
        // Exact wins over a newer wildcard.
        assert_eq!(
            select_profile(&candidates, "com.sap.apeirora.idtx.vision"),
            Some(PathBuf::from("/exact.mobileprovision"))
        );
        // A bundle only the wildcard covers falls back to it.
        assert_eq!(
            select_profile(&candidates, "com.other.app"),
            Some(PathBuf::from("/wild.mobileprovision"))
        );
    }

    #[test]
    fn select_newest_among_wildcards() {
        let older = MobileProvision {
            team_id: "T".into(),
            app_id: "T.*".into(),
            bundle_id: "*".into(),
            provisioned_devices: vec![],
            expiration: Some("2026-01-01T00:00:00Z".into()),
        };
        let newer = MobileProvision {
            expiration: Some("2026-06-01T00:00:00Z".into()),
            ..older.clone()
        };
        let candidates = vec![
            (PathBuf::from("/older.mobileprovision"), older),
            (PathBuf::from("/newer.mobileprovision"), newer),
        ];
        assert_eq!(
            select_profile(&candidates, "whatever"),
            Some(PathBuf::from("/newer.mobileprovision"))
        );
    }
}
