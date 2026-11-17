//! Content identity for a resolved guest build. Cargo remains the authority for
//! dynamically discovered include!/env! inputs; it is invoked on every build.

use std::collections::{BTreeSet, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::{BuildError, Result, compile, skeleton};

pub(crate) struct Fingerprint(Sha256);

impl Fingerprint {
    pub(crate) fn new() -> Self { Self(Sha256::new()) }

    pub(crate) fn add(&mut self, key: &str, bytes: &[u8]) {
        self.0.update((key.len() as u64).to_le_bytes());
        self.0.update(key.as_bytes());
        self.0.update((bytes.len() as u64).to_le_bytes());
        self.0.update(bytes);
    }

    fn file(&mut self, label: &Path, path: &Path) -> Result<()> {
        self.add("path", label.as_os_str().as_encoded_bytes());
        let mut file = std::fs::File::open(path)?;
        let expected = file.metadata()?.len();
        self.0.update(expected.to_le_bytes());
        let mut buffer = [0; 8192];
        let mut bytes = 0_u64;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 { break; }
            self.0.update(&buffer[..read]);
            bytes += read as u64;
        }
        if bytes != expected { return Err(BuildError::InputsChanged); }
        Ok(())
    }

    pub(crate) fn finish(self) -> String {
        use std::fmt::Write as _;
        self.0.finalize().iter().fold(String::with_capacity(64), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        })
    }
}

pub(crate) fn file_hash(path: &Path) -> Result<String> {
    let mut hash = Fingerprint::new();
    hash.file(Path::new("artifact"), path)?;
    Ok(hash.finish())
}

/// Workspace inheritance and its lock seed belong to the selected SDK, not the
/// build tool's package version or a hard-coded checkout location.
pub(crate) fn workspace_manifest(package_dir: &Path) -> Result<PathBuf> {
    let manifest = package_dir.join("Cargo.toml");
    let package: toml::Value = toml::from_str(&std::fs::read_to_string(&manifest)?)
        .map_err(|e| BuildError::InvalidInput(format!("invalid SDK manifest: {e}")))?;
    if let Some(workspace) = package.get("package").and_then(|p| p.get("workspace")).and_then(toml::Value::as_str) {
        return Ok(std::fs::canonicalize(package_dir.join(workspace).join("Cargo.toml"))?);
    }
    for dir in package_dir.ancestors() {
        let candidate = dir.join("Cargo.toml");
        let text = match std::fs::read_to_string(&candidate) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let value: toml::Value = toml::from_str(&text).map_err(|e| BuildError::InvalidInput(e.to_string()))?;
        if value.get("workspace").is_some() { return Ok(candidate); }
    }
    Ok(manifest)
}

pub(crate) fn seed_lock(scratch: &Path, sdk_workspace: &Path) -> Result<()> {
    let seed = sdk_workspace.with_file_name("Cargo.lock");
    let bytes = match std::fs::read(&seed) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let mut hash = Fingerprint::new();
    hash.add("lock-seed", bytes.as_deref().unwrap_or(b"<no seed>"));
    let identity = hash.finish();
    let old = std::fs::read_to_string(scratch.join("lock-seed.sha256")).ok();
    if old.as_deref() != Some(&identity) || !scratch.join("Cargo.lock").is_file() {
        if let Some(bytes) = bytes {
            skeleton::write_if_changed(&scratch.join("Cargo.lock"), &bytes)?;
        } else if old.is_some() {
            match std::fs::remove_file(scratch.join("Cargo.lock")) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        skeleton::write_if_changed(&scratch.join("lock-seed.sha256"), identity.as_bytes())?;
    }
    Ok(())
}

fn hash_tree(hash: &mut Fingerprint, root: &Path, dir: &Path, excluded: &[PathBuf], active: &mut HashSet<PathBuf>) -> Result<()> {
    let canonical = std::fs::canonicalize(dir)?;
    if !active.insert(canonical.clone()) {
        return Err(BuildError::InvalidInput(format!("cyclic SDK source directory: {}", dir.display())));
    }
    let mut entries = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if excluded.iter().any(|p| path.starts_with(p)) { continue; }
        let name = entry.file_name();
        if ["target", ".git", ".hg", ".svn"].iter().any(|skip| name == *skip) { continue; }
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            hash.add("symlink", std::fs::read_link(&path)?.as_os_str().as_encoded_bytes());
        }
        let metadata = std::fs::metadata(&path)?;
        if metadata.is_dir() {
            hash_tree(hash, root, &path, excluded, active)?;
        } else if metadata.is_file() {
            hash.file(path.strip_prefix(root).map_err(|e| BuildError::InvalidInput(e.to_string()))?, &path)?;
        } else {
            return Err(BuildError::InvalidInput(format!("unsupported SDK source entry: {}", path.display())));
        }
    }
    active.remove(&canonical);
    Ok(())
}

fn hash_configuration(hash: &mut Fingerprint, scratch: &Path) -> Result<()> {
    let mut candidates: BTreeSet<_> = compile::config_files(scratch).into_iter().collect();
    for ancestor in scratch.ancestors() {
        for name in ["rust-toolchain", "rust-toolchain.toml"] {
            candidates.insert(ancestor.join(name));
        }
    }
    for path in candidates {
        match std::fs::metadata(&path) {
            Ok(_) => hash.file(&path, &path)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    let mut environment: Vec<_> = std::env::vars_os().filter(|(key, _)| {
        let key = key.to_string_lossy();
        key.starts_with("CARGO_PROFILE_RELEASE_") || key.starts_with("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_")
            || ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC", "RUSTUP_TOOLCHAIN", "RUSTC_WRAPPER",
                "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTFLAGS", "CARGO_BUILD_RUSTC", "CARGO_HOME"].contains(&key.as_ref())
    }).collect();
    environment.sort();
    for (key, value) in environment {
        hash.add("environment-key", key.as_encoded_bytes());
        hash.add("environment-value", value.as_encoded_bytes());
        if (key == "RUSTC_WRAPPER" || key == "RUSTC_WORKSPACE_WRAPPER")
            && let Some(path) = compile::find_program(&value, scratch) {
            hash.file(&path, &path)?;
        }
    }
    Ok(())
}

/// All resolved package content (including transitive ABI/macros and registry
/// dependencies), the resolved lock, compiler identities, and the build recipe.
pub(crate) fn build_hash(scratch: &Path, metadata: &serde_json::Value, tools: &compile::Toolchain, excluded: &[PathBuf]) -> Result<String> {
    let mut hash = Fingerprint::new();
    hash.add("format", b"idtx-behavior-inputs-v2");
    for recipe in [include_str!("lib.rs"), include_str!("compile.rs"), include_str!("skeleton.rs"), include_str!("fingerprint.rs")] {
        hash.add("build-recipe", recipe.as_bytes());
    }
    for template in skeleton::recipe_inputs() { hash.add("skeleton", template.as_bytes()); }
    hash.add("target", compile::WASM_TARGET.as_bytes());
    hash.add("tools", &serde_json::to_vec(tools).map_err(|e| BuildError::InvalidInput(e.to_string()))?);
    hash_configuration(&mut hash, scratch)?;
    hash.file(Path::new("resolved/Cargo.lock"), &scratch.join("Cargo.lock"))?;
    let root = metadata["resolve"]["root"].as_str().ok_or_else(|| BuildError::InvalidInput("Cargo did not identify the guest package".into()))?;
    let nodes = metadata["resolve"]["nodes"].as_array().ok_or_else(|| BuildError::InvalidInput("Cargo returned no dependency graph".into()))?;
    let packages = metadata["packages"].as_array().ok_or_else(|| BuildError::InvalidInput("Cargo returned no packages".into()))?;
    let mut reachable = BTreeSet::new();
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        if !reachable.insert(id) { continue; }
        let node = nodes.iter().find(|n| n["id"] == id).ok_or_else(|| BuildError::InvalidInput("incomplete dependency graph".into()))?;
        for dep in node["dependencies"].as_array().into_iter().flatten() {
            pending.push(dep.as_str().ok_or_else(|| BuildError::InvalidInput("invalid dependency id".into()))?);
        }
    }
    for id in reachable {
        let package = packages.iter().find(|p| p["id"] == id).ok_or_else(|| BuildError::InvalidInput("missing resolved package".into()))?;
        let node = nodes.iter().find(|n| n["id"] == id).ok_or_else(|| BuildError::InvalidInput("missing resolved node".into()))?;
        hash.add("package", id.as_bytes());
        hash.add("features", &serde_json::to_vec(&node["features"]).map_err(|e| BuildError::InvalidInput(e.to_string()))?);
        let manifest = Path::new(package["manifest_path"].as_str().ok_or_else(|| BuildError::InvalidInput("missing package manifest path".into()))?);
        if id == root {
            hash.file(Path::new("Cargo.toml"), &scratch.join("Cargo.toml"))?;
            hash_tree(&mut hash, scratch, &scratch.join("src"), excluded, &mut HashSet::new())?;
        } else {
            let directory = manifest.parent().ok_or_else(|| BuildError::InvalidInput("missing package directory".into()))?;
            if excluded.iter().any(|path| directory.starts_with(path)) {
                return Err(BuildError::InvalidInput("SDK dependency is inside generated build-cache storage".into()));
            }
            hash_tree(&mut hash, directory, directory, excluded, &mut HashSet::new())?;
            if package["source"].is_null() {
                let workspace = workspace_manifest(directory)?;
                hash.file(&workspace, &workspace)?;
            }
        }
    }
    Ok(hash.finish())
}
