//! Dependency pins come from Cargo's resolved lockfile edges. Native runtimes
//! embed a build record, allowing packagers to inspect foreign binaries without
//! executing them or trusting an independently copied sidecar.

use std::io::{self, Read, Seek};
use std::path::Path;

use serde::{Deserialize, Serialize};

const MAGIC: &[u8] = b"\0IDTX-RUNTIME-METADATA:1\0";
const MAX_RECORD_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyVersions {
    pub glam: String,
    pub wgpu: String,
    pub wasmtime: String,
}

/// The effective build settings of the selected native runtime, not the cook
/// host's guesses or a list of features that somebody intended to build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeBuildMetadata {
    pub schema: u32,
    pub target: String,
    pub profile: String,
    pub rustc: String,
    pub package_version: String,
    pub abi_version: u32,
    pub dependencies: DependencyVersions,
    pub features: Vec<String>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Follow the dependency edge, rather than taking the first package with a
/// matching name: the workspace legitimately resolves multiple glam versions.
pub fn versions_from_lock(text: &str) -> io::Result<DependencyVersions> {
    let lock: toml::Value = toml::from_str(text).map_err(|e| invalid(format!("invalid Cargo.lock: {e}")))?;
    let packages = lock.get("package").and_then(toml::Value::as_array).ok_or_else(|| invalid("Cargo.lock has no packages"))?;
    let dependency = |owner: &str, name: &str| -> io::Result<String> {
        let owners: Vec<_> = packages.iter().filter(|p| p.get("name").and_then(toml::Value::as_str) == Some(owner)
            && p.get("source").is_none()).collect();
        let [owner] = owners.as_slice() else { return Err(invalid("missing or ambiguous workspace dependency owner")); };
        let spec = owner.get("dependencies").and_then(toml::Value::as_array)
            .and_then(|deps| deps.iter().filter_map(toml::Value::as_str).find(|dep| dep.split_whitespace().next() == Some(name)))
            .ok_or_else(|| invalid(format!("missing resolved {name} dependency")))?;
        let mut words = spec.split_whitespace();
        words.next();
        let version = words.next();
        let source = words.next().map(|s| s.trim_start_matches('(').trim_end_matches(')'));
        let candidates: Vec<_> = packages.iter().filter(|p| {
            p.get("name").and_then(toml::Value::as_str) == Some(name)
                && version.is_none_or(|v| p.get("version").and_then(toml::Value::as_str) == Some(v))
                && source.is_none_or(|s| p.get("source").and_then(toml::Value::as_str) == Some(s))
        }).collect();
        let [package] = candidates.as_slice() else { return Err(invalid(format!("ambiguous resolved {name} dependency"))); };
        package.get("version").and_then(toml::Value::as_str).map(str::to_owned)
            .ok_or_else(|| invalid(format!("resolved {name} has no version")))
    };
    Ok(DependencyVersions {
        glam: dependency("idtx-behavior-abi", "glam")?,
        wgpu: dependency("idtx-render", "wgpu")?,
        wasmtime: dependency("idtx-behavior", "wasmtime")?,
    })
}

/// Build-script helper. Cargo establishes the lockfile before running build
/// scripts; changing a selected version invalidates the emitted constants.
pub fn emit_dependency_versions(workspace: &Path) -> io::Result<DependencyVersions> {
    let lock = workspace.join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock.display());
    let versions = versions_from_lock(&std::fs::read_to_string(lock)?)?;
    println!("cargo:rustc-env=IDTX_PIN_GLAM={}", versions.glam);
    println!("cargo:rustc-env=IDTX_PIN_WGPU={}", versions.wgpu);
    println!("cargo:rustc-env=IDTX_PIN_WASMTIME={}", versions.wasmtime);
    Ok(versions)
}

impl RuntimeBuildMetadata {
    /// Encode the record for embedding in a binary. A fixed magic plus length
    /// allows bounded streaming inspection even of a very large executable.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let body = serde_json::to_vec(self).map_err(io::Error::other)?;
        if body.len() > MAX_RECORD_BYTES { return Err(invalid("runtime build record is oversized")); }
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&u32::try_from(body.len()).map_err(|_| invalid("runtime build record is oversized"))?.to_le_bytes());
        bytes.extend_from_slice(&body);
        Ok(bytes)
    }

    pub fn read_binary(path: &Path) -> io::Result<Self> {
        Self::read_from(&mut std::fs::File::open(path)?)
    }

    fn read_from(input: &mut (impl Read + Seek)) -> io::Result<Self> {
        let mut buffer = vec![0; MAX_RECORD_BYTES];
        let mut kept = 0;
        let mut offset = 0_u64;
        loop {
            let read = input.read(&mut buffer[kept..])?;
            if read == 0 { return Err(invalid("runtime has no embedded build metadata; rebuild it")); }
            let end = kept + read;
            if let Some(index) = buffer[..end].windows(MAGIC.len()).position(|w| w == MAGIC) {
                input.seek(io::SeekFrom::Start(offset + index as u64 + MAGIC.len() as u64))?;
                let mut length = [0; 4];
                input.read_exact(&mut length)?;
                let length = u32::from_le_bytes(length) as usize;
                if length > MAX_RECORD_BYTES { return Err(invalid("runtime build record is oversized")); }
                let mut body = vec![0; length];
                input.read_exact(&mut body)?;
                let record: Self = serde_json::from_slice(&body).map_err(io::Error::other)?;
                if record.schema != 1 { return Err(invalid("unsupported runtime build metadata schema")); }
                return Ok(record);
            }
            kept = end.min(MAGIC.len() - 1);
            buffer.copy_within(end - kept..end, 0);
            offset += (end - kept) as u64;
        }
    }
}

/// Called from the native runtime's build script. Features are captured from
/// Cargo's effective `CARGO_FEATURE_*` set, including defaults and implications.
pub fn emit_runtime_metadata(workspace: &Path, manifest_dir: &Path, abi_version: u32) -> io::Result<()> {
    let dependencies = emit_dependency_versions(workspace)?;
    let manifest = manifest_dir.join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let manifest: toml::Value = toml::from_str(&std::fs::read_to_string(manifest)?).map_err(io::Error::other)?;
    let declared = manifest.get("features").and_then(toml::Value::as_table).ok_or_else(|| invalid("runtime has no feature table"))?;
    let mut features: Vec<String> = std::env::vars().filter_map(|(key, _)| {
        let suffix = key.strip_prefix("CARGO_FEATURE_")?;
        Some(declared.keys().find(|name| name.replace('-', "_").to_ascii_uppercase() == suffix)
            .cloned().unwrap_or_else(|| suffix.to_ascii_lowercase().replace('_', "-")))
    }).collect();
    features.sort();
    features.dedup();
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let version = std::process::Command::new(rustc).arg("-vV").output()?;
    if !version.status.success() { return Err(invalid("could not identify runtime compiler")); }
    let required_env = |name: &str| std::env::var(name).map_err(|e| invalid(format!("{name}: {e}")));
    let record = RuntimeBuildMetadata {
        schema: 1,
        target: required_env("TARGET")?,
        profile: required_env("PROFILE")?,
        rustc: String::from_utf8(version.stdout).map_err(io::Error::other)?,
        package_version: required_env("CARGO_PKG_VERSION")?,
        abi_version,
        dependencies,
        features,
    };
    std::fs::write(Path::new(&required_env("OUT_DIR")?).join("runtime-build.bin"), record.encode()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> RuntimeBuildMetadata {
        RuntimeBuildMetadata { schema: 1, target: "aarch64-apple-visionos".into(), profile: "release".into(),
            rustc: "rustc fixture".into(), package_version: "1".into(), abi_version: 3,
            dependencies: DependencyVersions { glam: "0.33.12".into(), wgpu: "29.0.4".into(), wasmtime: "48.0.1".into() },
            features: vec!["remote".into(), "visionos-immersive".into()] }
    }

    #[test]
    fn reads_foreign_binary_record_across_chunk_boundaries() {
        let expected = record();
        for padding in [0, MAX_RECORD_BYTES - 5, 3 * MAX_RECORD_BYTES] {
            let mut bytes = vec![0x55; padding];
            bytes.extend(expected.encode().unwrap());
            bytes.extend([0x44; 200]);
            assert_eq!(RuntimeBuildMetadata::read_from(&mut io::Cursor::new(bytes)).unwrap(), expected);
        }
    }

    #[test]
    fn rejects_absent_truncated_and_oversized_records() {
        assert!(RuntimeBuildMetadata::read_from(&mut io::Cursor::new(b"old runtime")).is_err());
        let mut short = record().encode().unwrap();
        short.truncate(short.len() - 1);
        assert!(RuntimeBuildMetadata::read_from(&mut io::Cursor::new(short)).is_err());
        let mut large = MAGIC.to_vec();
        large.extend(u32::MAX.to_le_bytes());
        assert!(RuntimeBuildMetadata::read_from(&mut io::Cursor::new(large)).is_err());
    }

    #[test]
    fn dependency_owner_selects_the_right_glam_among_multiple_versions() {
        let text = r#"
[[package]]
name = "idtx-behavior-abi"
version = "1"
dependencies = ["glam 0.33.12"]
[[package]]
name = "idtx-render"
version = "1"
dependencies = ["wgpu"]
[[package]]
name = "idtx-behavior"
version = "1"
dependencies = ["wasmtime"]
[[package]]
name = "glam"
version = "0.30.0"
[[package]]
name = "glam"
version = "0.33.12"
[[package]]
name = "wgpu"
version = "29.0.4"
[[package]]
name = "wasmtime"
version = "48.0.1"
"#;
        assert_eq!(versions_from_lock(text).unwrap(), record().dependencies);
        assert!(versions_from_lock(&text.replace("glam 0.33.12", "glam")).is_err());
    }
}
