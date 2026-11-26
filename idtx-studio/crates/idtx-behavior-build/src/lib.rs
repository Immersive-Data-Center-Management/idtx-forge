//! Compile a behavior source collection with a resolved, content-aware cache.
//!
//! The selected SDK and its transitive packages, lockfile, compiler/tooling,
//! Cargo configuration and embedded recipe form the input fingerprint. Cargo is
//! invoked even on reuse to validate dynamically discovered include!/env! inputs.
//! Successful cache receipts verify both compiler output and published WASM.

mod compile;
mod development;
mod fingerprint;
pub mod sdk_sysroot;
pub mod skeleton;
mod source;

use std::path::{Component, Path, PathBuf};

pub use compile::source_hash;
pub use development::{DevelopmentContext, DevelopmentOptions, prepare_development};
pub use sdk_sysroot::{SDK_ENV, SDK_SYSROOT_DIR, resolve_sdk_dir, stage_sdk};
pub use skeleton::{pascal_case, render_author_skeleton, synthesize_uuid};
pub use source::BehaviorSource;

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("source file not found: {0}")]
    SourceNotFound(PathBuf),
    #[error("SDK manifest not found: {0}")]
    SdkNotFound(PathBuf),
    #[error("invalid build input: {0}")]
    InvalidInput(String),
    #[error("build inputs changed during compilation; retry the build")]
    InputsChanged,
    #[error("cargo build failed:\n{0}")]
    CargoFailed(String),
    #[error("cargo could not be spawned: {0}")]
    CargoSpawn(String),
    #[error("expected wasm artifact missing at {0}")]
    ArtifactMissing(PathBuf),
}

pub type Result<T> = std::result::Result<T, BuildError>;

#[derive(Debug, Clone)]
pub struct CompiledArtifact {
    pub wasm_path: PathBuf,
    pub name: String,
    /// Cargo confirmed freshness and both cached files matched their receipts.
    pub cache_hit: bool,
    /// Identity of the resolved inputs AND final WASM bytes. Dynamic inputs
    /// discovered by Cargo cannot silently change bytes under this identity.
    pub hash: String,
}

#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub out_dir: PathBuf,
    /// Defaults to `<out_dir>/.behavior-cache`.
    pub cache_dir: Option<PathBuf>,
    /// Selected SDK crate directory; relative paths are resolved against the
    /// caller's working directory before materialising the Cargo dependency.
    pub sdk_path: PathBuf,
    /// Optional source collection. Defaults to the enclosing `behaviors/` tree,
    /// or the entry's parent directory when no such tree exists.
    pub source_root: Option<PathBuf>,
    /// Publication name override. The default is a readable, location-derived
    /// identity, so entries with identical filenames cannot overwrite each other.
    pub name: Option<String>,
}

impl BuildOptions {
    #[must_use]
    pub fn new(out_dir: impl Into<PathBuf>, sdk_path: impl Into<PathBuf>) -> Self {
        Self { out_dir: out_dir.into(), cache_dir: None, sdk_path: sdk_path.into(), source_root: None, name: None }
    }

    fn cache_root(&self) -> PathBuf {
        self.cache_dir.clone().unwrap_or_else(|| self.out_dir.join(".behavior-cache"))
    }
}

/// The canonical managed recipe used by both live development and snapshots.
/// Editor manifests are output, never an input to the production recipe.
struct Recipe {
    source: BehaviorSource,
    sdk: PathBuf,
    name: String,
    crate_name: String,
}

impl Recipe {
    fn resolve(entry: &Path, root: Option<&Path>, sdk: &Path, name: Option<&str>) -> Result<Self> {
        let source = BehaviorSource::discover(entry, root)?;
        let sdk_manifest = sdk.join("Cargo.toml");
        if !sdk_manifest.is_file() { return Err(BuildError::SdkNotFound(sdk_manifest)); }
        let sdk = simplified(std::fs::canonicalize(sdk)?);
        let manifest: toml::Value = toml::from_str(&std::fs::read_to_string(sdk.join("Cargo.toml"))?)
            .map_err(|e| BuildError::InvalidInput(e.to_string()))?;
        if manifest.get("package").and_then(|p| p.get("name")).and_then(toml::Value::as_str) != Some("idtx-behavior-sdk") {
            return Err(BuildError::InvalidInput("selected crate is not idtx-behavior-sdk".into()));
        }
        let name = name.map_or_else(|| source.name(), str::to_owned);
        if name.contains(['/', '\\']) || Path::new(&name).components().count() != 1
            || !matches!(Path::new(&name).components().next(), Some(Component::Normal(_))) {
            return Err(BuildError::InvalidInput("behavior name must be one file name".into()));
        }
        Ok(Self { crate_name: crate_name(&name), name, source, sdk })
    }

    fn workspace_key(&self) -> String {
        let mut identity = fingerprint::Fingerprint::new();
        identity.add("format", b"behavior-module-workspace-v1");
        identity.add("source-path", self.source.entry().as_os_str().as_encoded_bytes());
        identity.add("source-root", self.source.root().as_os_str().as_encoded_bytes());
        identity.add("sdk-path", self.sdk.as_os_str().as_encoded_bytes());
        identity.add("crate-name", self.crate_name.as_bytes());
        identity.finish()
    }

    fn materialise(&self, directory: &Path, entry: &Path) -> Result<()> {
        skeleton::materialise(directory, &self.crate_name, &self.sdk, entry)?;
        fingerprint::seed_lock(directory, &fingerprint::workspace_manifest(&self.sdk)?)
    }
}

fn lock_directory(directory: &Path, name: &str) -> Result<std::fs::File> {
    std::fs::create_dir_all(directory)?;
    let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .open(directory.join(name))?;
    lock.lock()?;
    Ok(lock)
}

struct Request {
    name: String,
    crate_name: String,
    scratch: PathBuf,
    cache: PathBuf,
    final_wasm: PathBuf,
    tools: compile::Toolchain,
    metadata: serde_json::Value,
    input_hash: String,
    source: BehaviorSource,
    source_hash: String,
    source_exclusions: Vec<PathBuf>,
    // Serialize skeleton/lock/receipt updates, including between editor processes.
    _lock: std::fs::File,
}

impl Request {
    fn prepare(src_file: &Path, opts: &BuildOptions) -> Result<Self> {
        let recipe = Recipe::resolve(src_file, opts.source_root.as_deref(), &opts.sdk_path, opts.name.as_deref())?;
        std::fs::create_dir_all(&opts.out_dir)?;
        std::fs::create_dir_all(opts.cache_root())?;
        let cache = simplified(std::fs::canonicalize(opts.cache_root())?);
        let out = simplified(std::fs::canonicalize(&opts.out_dir)?);
        let final_wasm = out.join(format!("{}.wasm", recipe.name));
        let scratch = cache.join("work").join(path_segment(&recipe.workspace_key()));
        let lock = lock_directory(&scratch, "build.lock")?;
        let mut source_exclusions = vec![cache.clone()];
        if !recipe.source.entry().starts_with(&out) { source_exclusions.push(out); }
        let snapshot = recipe.source.snapshot(&source_exclusions)?;
        let source_hash = snapshot.hash();
        snapshot.stage(&scratch.join("src/authored"))?;
        recipe.materialise(&scratch, &Path::new("authored").join(recipe.source.relative_entry()))?;
        let tools = compile::Toolchain::discover(&scratch)?;
        let metadata = compile::cargo_metadata(&scratch, &tools)?;
        let mut request = Self { name: recipe.name, crate_name: recipe.crate_name, scratch, cache, final_wasm, tools, metadata,
            source: recipe.source, source_hash, source_exclusions, input_hash: String::new(), _lock: lock };
        request.input_hash = request.fingerprint()?;
        Ok(request)
    }

    fn fingerprint(&self) -> Result<String> {
        fingerprint::build_hash(&self.scratch, &self.metadata, &self.tools,
            &[self.cache.join("work"), self.cache.join("artifacts"), self.final_wasm.clone()])
    }

    fn artifact_dir(&self) -> PathBuf { self.cache.join("artifacts").join(path_segment(&self.input_hash)) }

    fn validate_inputs(&self) -> Result<()> {
        if self.fingerprint()? != self.input_hash || self.source.snapshot(&self.source_exclusions)?.hash() != self.source_hash {
            return Err(BuildError::InputsChanged);
        }
        Ok(())
    }

    fn compile(&self, target: &Path, command: &str) -> Result<bool> {
        compile::cargo_wasm(&self.scratch, target, &self.crate_name, &self.tools, command).map_err(|error| {
            if let BuildError::CargoFailed(message) = error {
                // Sources are copied verbatim. Restore original author paths in
                // Conductor diagnostics without changing file!() or rustc flags.
                let root = self.source.root().to_string_lossy();
                let staged = self.scratch.join("src/authored");
                let message = message.replace(staged.to_string_lossy().as_ref(), &root)
                    .replace(&staged.to_string_lossy().replace('\\', "/"), &root)
                    .replace("src/authored/", &format!("{root}/"))
                    .replace("src\\authored\\", &format!("{root}\\"));
                BuildError::CargoFailed(message)
            } else { error }
        })
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Receipt {
    inputs: String,
    raw: String,
    wasm: String,
}

fn publish_copy(source: &Path, destination: &Path) -> Result<()> {
    let parent = destination.parent().ok_or_else(|| BuildError::InvalidInput("output has no parent".into()))?;
    std::fs::create_dir_all(parent)?;
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    std::fs::copy(source, temporary.path())?;
    temporary.persist(destination).map_err(|e| BuildError::Io(e.error))?;
    Ok(())
}

/// Resolve current inputs, ask Cargo to validate/rebuild them, and publish only
/// successful output. A bare artifact or old v1 source-only cache is never a hit.
pub fn build_behavior(src_file: &Path, opts: &BuildOptions) -> Result<CompiledArtifact> {
    #[cfg(feature = "metrics")]
    let started = std::time::Instant::now();
    let request = Request::prepare(src_file, opts)?;
    let directory = request.artifact_dir();
    let target = directory.join("target");
    let raw = compile::target_wasm_path(&target, &request.crate_name);
    let complete = directory.join("artifact.wasm");
    let receipt_path = directory.join("receipt.json");
    let previous: Option<Receipt> = std::fs::read(&receipt_path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let mut fresh = request.compile(&target, "build")?;
    let mut raw_hash = fingerprint::file_hash(&raw)?;
    if fresh && !previous.as_ref().is_some_and(|receipt| receipt.inputs == request.input_hash && receipt.raw == raw_hash) {
        // Cargo's freshness is not a checksum for a tampered/seeded output. A
        // missing or mismatched receipt requires a real rebuild of this key.
        std::fs::remove_dir_all(&target)?;
        request.compile(&target, "build")?;
        raw_hash = fingerprint::file_hash(&raw)?;
        fresh = false;
    }
    request.validate_inputs()?;
    let cached_output = previous.as_ref().is_some_and(|receipt| receipt.inputs == request.input_hash
        && receipt.raw == raw_hash && fingerprint::file_hash(&complete).ok().as_ref() == Some(&receipt.wasm));
    if !cached_output {
        let optimized = tempfile::NamedTempFile::new_in(&directory)?;
        let successful = compile::maybe_wasm_opt(&raw, optimized.path(), request.tools.optimizer.as_ref());
        publish_copy(if successful { optimized.path() } else { &raw }, &complete)?;
    }
    let output_hash = fingerprint::file_hash(&complete)?;
    let receipt = Receipt { inputs: request.input_hash.clone(), raw: raw_hash, wasm: output_hash.clone() };
    let receipt_bytes = serde_json::to_vec(&receipt).map_err(|e| BuildError::InvalidInput(e.to_string()))?;
    let temporary = tempfile::NamedTempFile::new_in(&directory)?;
    std::fs::write(temporary.path(), receipt_bytes)?;
    temporary.persist(&receipt_path).map_err(|e| BuildError::Io(e.error))?;
    publish_copy(&complete, &request.final_wasm)?;
    let mut identity = fingerprint::Fingerprint::new();
    identity.add("inputs", request.input_hash.as_bytes());
    identity.add("wasm", output_hash.as_bytes());
    let cache_hit = fresh && cached_output;
    #[cfg(feature = "metrics")]
    tracing::info!(name = %request.name, cache_hit, ms = started.elapsed().as_millis(), "behavior build");
    Ok(CompiledArtifact { wasm_path: request.final_wasm, name: request.name, cache_hit, hash: identity.finish() })
}

/// Check against the same resolved inputs and locked dependency set as a build.
pub fn check_behavior(src_file: &Path, opts: &BuildOptions) -> Result<()> {
    let request = Request::prepare(src_file, opts)?;
    request.compile(&request.artifact_dir().join("target"), "check")?;
    request.validate_inputs()?;
    Ok(())
}

/// Source-only UI drift indicator. It is deliberately not a build-cache key;
/// [`build_behavior`] resolves and checks the SDK/compiler/dependency inputs.
#[must_use]
pub fn behavior_source_hash(source: &str) -> String { compile::source_hash(source, "authored-source") }

fn crate_name(name: &str) -> String {
    let mut s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    if s.is_empty() || s.chars().next().is_some_and(|c| c.is_ascii_digit()) { s.insert_str(0, "behavior_"); }
    s
}

/// Length of a cache digest when it is used as a filesystem path segment.
///
/// The full 64-hex digest still keys the cache — it is written into each
/// `receipt.json` and re-verified on reuse (`receipt.inputs == input_hash`) — so
/// a shorter *directory name* can never authorise an incorrect cache hit: an
/// (astronomically unlikely) 64-bit prefix collision only forces a rebuild of
/// that key. Truncating keeps the nested `artifacts/<hash>/target/release/build/
/// <crate>-<hash>/build_script_build-<hash>.exe` paths well under Windows'
/// `MAX_PATH` (260), where MSVC `link.exe` — which is not long-path aware, even
/// with `LongPathsEnabled` set — otherwise fails with `LNK1104` when a
/// build-script executable's path crosses the limit.
const PATH_SEGMENT_LEN: usize = 16;

fn path_segment(hash: &str) -> &str {
    &hash[..hash.len().min(PATH_SEGMENT_LEN)]
}

/// Strip the Windows `\\?\` verbatim prefix that [`std::fs::canonicalize`] adds,
/// so paths handed to cargo/rustc reach MSVC `link.exe` in legacy form.
///
/// `link.exe` cannot open verbatim-prefixed *input* files — it reports
/// `LNK1181: cannot open input file '.obj'` on the linker's `symbols.o`, even
/// when the path is well within `MAX_PATH`. Once [`path_segment`] keeps the
/// cache paths short, the `\\?\` prefix is no longer needed, so dropping it lets
/// the legacy path APIs (which `link.exe` uses) resolve the file.
///
/// Only the `\\?\<drive>:\…` disk form is simplified; UNC verbatim paths
/// (`\\?\UNC\…`) are left untouched because they still require the prefix.
/// No-op on non-Windows.
fn simplified(path: std::path::PathBuf) -> std::path::PathBuf {
    #[cfg(windows)]
    if let Some(rest) = path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        let bytes = rest.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            return std::path::PathBuf::from(rest);
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_sanitises() {
        assert_eq!(crate_name("spinning-cube"), "spinning_cube");
        assert_eq!(crate_name("3d"), "behavior_3d");
        assert_eq!(crate_name(""), "behavior_");
        assert_eq!(crate_name("ok_name"), "ok_name");
    }

    #[test]
    fn missing_source_errors() {
        let opts = BuildOptions::new("unused-out", "unused-sdk");
        assert!(matches!(build_behavior(Path::new("/no/such/file.rs"), &opts), Err(BuildError::SourceNotFound(_))));
        assert!(matches!(check_behavior(Path::new("/no/such/file.rs"), &opts), Err(BuildError::SourceNotFound(_))));
    }
}
