//! Frozen application extension kernel and manifest contracts for IDTX Studio.
//!
//! An IDTX *extension* is a unit of third-party (or first-party) functionality
//! mounted into the studio: a sandboxed WASM behavior, an editor panel, or a
//! privileged host extension. This crate freezes the **contracts** that gate and
//! identify such extensions — deliberately UI- and host-neutral so the same
//! manifest is understood by the conductor, the packaged runtime, and the build
//! pipeline alike.
//!
//! The two axes an extension is classified along are:
//!
//! - its [`TrustClass`] — the taxonomy that decides whether it may request host
//!   capabilities at all, and
//! - its [`ExtensionKind`] — what shape of thing it is (behavior / editor plugin
//!   / host extension).
//!
//! An [`ExtensionManifest`] (an `extension.toml`) binds a stable
//! [`ExtensionIdentity`] to a trust class, a kind, and the concrete list of host
//! capability tokens the extension requests. [`ExtensionManifest::validate`]
//! enforces the core invariant that a [`Sandboxed`](TrustClass::Sandboxed)
//! extension may not request any host capabilities.
//!
//! # Open design question (v1)
//!
//! Whether a content hash is a *hard* requirement for a v1 manifest is still
//! open — see [`ExtensionIdentity::content_hash`]. For now it is optional so that
//! development / unsigned manifests load, while signed distribution channels can
//! require it out-of-band.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Highest extension manifest schema version this build understands.
pub const EXTENSION_MANIFEST_VERSION: u32 = 1;

/// Errors from loading or validating an [`ExtensionManifest`].
#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    /// The manifest file could not be read.
    #[error("read extension manifest {path}: {source}")]
    Io {
        /// The manifest path that failed to read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The manifest file was not valid TOML for this schema.
    #[error("parse extension manifest {path}: {source}")]
    Parse {
        /// The manifest path that failed to parse.
        path: PathBuf,
        /// The underlying TOML error.
        source: toml::de::Error,
    },
    /// The manifest declares a schema version newer than this build supports.
    #[error(
        "unsupported extension manifest version {found} (this build supports up to {supported})"
    )]
    UnsupportedVersion {
        /// The version found in the manifest.
        found: u32,
        /// The highest version this build understands.
        supported: u32,
    },
    /// The manifest requests host capabilities under a trust class that is not
    /// permitted to hold any.
    #[error(
        "trust class {trust:?} may not request host capabilities, but the manifest declares {count}"
    )]
    CapabilitiesRequireTrust {
        /// The offending (insufficiently privileged) trust class.
        trust: TrustClass,
        /// How many capability tokens the manifest declared.
        count: usize,
    },
}

/// The extension trust taxonomy — the privilege ceiling an extension is granted,
/// ordered least → most privileged.
///
/// This is the single gate on whether an extension may request host
/// capabilities: only [`Trusted`](Self::Trusted) and [`System`](Self::System)
/// extensions may, and [`Sandboxed`](Self::Sandboxed) may not (see
/// [`Self::allows_host_capabilities`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustClass {
    /// Pure, sandboxed WASM behavior with no access to host capabilities. The
    /// default and safest class — the only thing such an extension can do is run
    /// deterministic guest code against the behavior ABI.
    #[default]
    Sandboxed,
    /// Signed / first-party extension that is allowed to *request* host
    /// capabilities (filesystem, network, native panels). Still subject to the
    /// user granting the requested capability tokens.
    Trusted,
    /// Built-in / core extension shipped as part of the studio itself. Implicitly
    /// trusted and may request any host capability.
    System,
}

impl TrustClass {
    /// Whether an extension of this trust class may request host capabilities at
    /// all. `false` for [`Sandboxed`](Self::Sandboxed); `true` otherwise.
    #[must_use]
    pub fn allows_host_capabilities(self) -> bool {
        !matches!(self, Self::Sandboxed)
    }
}

/// What shape of extension a manifest describes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionKind {
    /// A WASM behavior module hosted by `idtx-behavior`.
    #[default]
    Behavior,
    /// An editor-side plugin contributing panels / commands to the UI.
    EditorPlugin,
    /// A privileged host-side extension that integrates with the native host.
    HostExtension,
}

/// The stable identity contract for an extension.
///
/// The pair `(id, version)` is the durable coordinate an extension is referenced
/// by; see [`Self::locator`]. The optional [`content_hash`](Self::content_hash)
/// pins the exact payload bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionIdentity {
    /// Stable reverse-DNS-ish identifier (e.g. `com.vendor.my_behavior`).
    #[serde(default)]
    pub id: String,
    /// Semantic version string of this extension release (e.g. `1.2.0`).
    #[serde(default)]
    pub version: String,
    /// SHA-256 hex of the extension payload, when known.
    ///
    /// Optional in v1: whether a content hash is a *hard* requirement is still an
    /// open design question. Unsigned / development manifests may omit it; signed
    /// distribution channels may require it out-of-band.
    #[serde(default)]
    pub content_hash: Option<String>,
}

impl ExtensionIdentity {
    /// The `"{id}@{version}"` locator used to reference this extension in logs,
    /// project extension-locks, and diagnostics.
    #[must_use]
    pub fn locator(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }
}

/// A parsed extension manifest (`extension.toml`). Constructed via
/// [`ExtensionManifest::load`], or synthesized with
/// [`ExtensionManifest::default`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionManifest {
    /// Schema version; see [`EXTENSION_MANIFEST_VERSION`].
    #[serde(default = "default_version")]
    pub version: u32,
    /// The stable identity of this extension.
    #[serde(default)]
    pub identity: ExtensionIdentity,
    /// What shape of extension this is.
    #[serde(default)]
    pub kind: ExtensionKind,
    /// The privilege ceiling this extension runs under.
    #[serde(default)]
    pub trust: TrustClass,
    /// Human-readable display name.
    #[serde(default)]
    pub display_name: String,
    /// Requested host capability tokens. Must be empty unless
    /// [`trust.allows_host_capabilities()`](TrustClass::allows_host_capabilities).
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Relative path to the module / plugin payload (e.g. `behavior.wasm`).
    #[serde(default)]
    pub entry: Option<PathBuf>,
}

fn default_version() -> u32 {
    EXTENSION_MANIFEST_VERSION
}

impl Default for ExtensionManifest {
    fn default() -> Self {
        Self {
            version: EXTENSION_MANIFEST_VERSION,
            identity: ExtensionIdentity::default(),
            kind: ExtensionKind::default(),
            trust: TrustClass::default(),
            display_name: String::new(),
            capabilities: Vec::new(),
            entry: None,
        }
    }
}

impl ExtensionManifest {
    /// Load and validate a manifest from a TOML file.
    ///
    /// # Errors
    ///
    /// Returns [`ExtensionError`] when the file cannot be read, is not valid TOML
    /// for this schema, declares an unsupported schema version, or fails
    /// [`validate`](Self::validate).
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ExtensionError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ExtensionError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let manifest: ExtensionManifest =
            toml::from_str(&text).map_err(|source| ExtensionError::Parse {
                path: path.to_path_buf(),
                source,
            })?;
        if manifest.version > EXTENSION_MANIFEST_VERSION {
            return Err(ExtensionError::UnsupportedVersion {
                found: manifest.version,
                supported: EXTENSION_MANIFEST_VERSION,
            });
        }
        manifest.validate()?;
        Ok(manifest)
    }

    /// Enforce the manifest's cross-field invariants.
    ///
    /// The core invariant: an extension may only declare host
    /// [`capabilities`](Self::capabilities) when its [`trust`](Self::trust) class
    /// [`allows_host_capabilities`](TrustClass::allows_host_capabilities). A
    /// [`Sandboxed`](TrustClass::Sandboxed) manifest requesting any capability is
    /// rejected.
    ///
    /// # Errors
    ///
    /// Returns [`ExtensionError::CapabilitiesRequireTrust`] when a trust class
    /// that forbids host capabilities declares one or more.
    pub fn validate(&self) -> Result<(), ExtensionError> {
        if !self.capabilities.is_empty() && !self.trust.allows_host_capabilities() {
            return Err(ExtensionError::CapabilitiesRequireTrust {
                trust: self.trust,
                count: self.capabilities.len(),
            });
        }
        Ok(())
    }

    /// Serialize this manifest back to pretty TOML (for logging / packaging).
    #[must_use]
    pub fn to_toml_string(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// A short content hash (first 8 bytes of SHA-256 over the TOML form), handy
    /// for correlating logs with a specific manifest.
    #[must_use]
    pub fn content_hash(&self) -> String {
        use std::fmt::Write as _;
        let mut hasher = Sha256::new();
        hasher.update(self.to_toml_string().as_bytes());
        hasher
            .finalize()
            .iter()
            .take(8)
            .fold(String::new(), |mut acc, b| {
                let _ = write!(acc, "{b:02x}");
                acc
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch dir under the system temp dir (no `tempfile` dep).
    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "idtx-extension-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn default_manifest_is_valid() {
        let manifest = ExtensionManifest::default();
        assert_eq!(manifest.version, EXTENSION_MANIFEST_VERSION);
        assert_eq!(manifest.trust, TrustClass::Sandboxed);
        assert_eq!(manifest.kind, ExtensionKind::Behavior);
        assert!(manifest.capabilities.is_empty());
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn sandboxed_disallows_host_capabilities() {
        assert!(!TrustClass::Sandboxed.allows_host_capabilities());
        assert!(TrustClass::Trusted.allows_host_capabilities());
        assert!(TrustClass::System.allows_host_capabilities());
    }

    #[test]
    fn toml_round_trips() {
        let manifest = ExtensionManifest {
            version: EXTENSION_MANIFEST_VERSION,
            identity: ExtensionIdentity {
                id: "com.vendor.my_behavior".to_string(),
                version: "1.2.0".to_string(),
                content_hash: Some("deadbeef".to_string()),
            },
            kind: ExtensionKind::EditorPlugin,
            trust: TrustClass::Trusted,
            display_name: "My Behavior".to_string(),
            capabilities: vec!["fs.read".to_string(), "net.fetch".to_string()],
            entry: Some(PathBuf::from("plugin.wasm")),
        };
        let text = manifest.to_toml_string();
        let back = toml::from_str::<ExtensionManifest>(&text).unwrap();
        assert_eq!(manifest, back);
    }

    #[test]
    fn rejects_future_version() {
        let dir = scratch("ver");
        let path = dir.join("extension.toml");
        std::fs::write(
            &path,
            format!("version = {}\n", EXTENSION_MANIFEST_VERSION + 1),
        )
        .unwrap();
        let err = ExtensionManifest::load(&path).unwrap_err();
        assert!(matches!(err, ExtensionError::UnsupportedVersion { .. }));
    }

    #[test]
    fn content_hash_is_stable_hex() {
        let manifest = ExtensionManifest::default();
        let first = manifest.content_hash();
        let second = manifest.content_hash();
        assert_eq!(first, second);
        // First 8 bytes → 16 lowercase hex chars.
        assert_eq!(first.len(), 16);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn validate_rejects_sandboxed_with_capabilities() {
        let manifest = ExtensionManifest {
            trust: TrustClass::Sandboxed,
            capabilities: vec!["fs.read".to_string()],
            ..ExtensionManifest::default()
        };
        let err = manifest.validate().unwrap_err();
        assert!(matches!(
            err,
            ExtensionError::CapabilitiesRequireTrust {
                trust: TrustClass::Sandboxed,
                count: 1,
            }
        ));
    }

    #[test]
    fn validate_allows_trusted_with_capabilities() {
        let manifest = ExtensionManifest {
            trust: TrustClass::Trusted,
            capabilities: vec!["fs.read".to_string()],
            ..ExtensionManifest::default()
        };
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn load_rejects_sandboxed_with_capabilities() {
        let dir = scratch("load-cap");
        let path = dir.join("extension.toml");
        std::fs::write(
            &path,
            "trust = \"sandboxed\"\ncapabilities = [\"fs.read\"]\n",
        )
        .unwrap();
        let err = ExtensionManifest::load(&path).unwrap_err();
        assert!(matches!(
            err,
            ExtensionError::CapabilitiesRequireTrust { .. }
        ));
    }

    #[test]
    fn locator_formats_id_and_version() {
        let identity = ExtensionIdentity {
            id: "com.vendor.thing".to_string(),
            version: "0.4.1".to_string(),
            content_hash: None,
        };
        assert_eq!(identity.locator(), "com.vendor.thing@0.4.1");
    }
}
