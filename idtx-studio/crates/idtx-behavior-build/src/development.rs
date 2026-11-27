//! Persistent, live-source Cargo contexts. This API has no editor/UI dependency;
//! an adapter can use it for VS Code, another IDE, or a language-server client.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{BehaviorSource, BuildError, Recipe, Result, compile, lock_directory, path_segment, simplified, skeleton::path_text};

#[derive(Debug, Clone)]
pub struct DevelopmentOptions {
    /// Managed storage, separate from the production cache. Context identities
    /// depend on source location and recipe, never source contents or prims.
    pub context_dir: PathBuf,
    pub sdk_path: PathBuf,
    pub source_root: Option<PathBuf>,
    /// Re-resolve the managed lockfile from the selected SDK's seed during an
    /// explicit repair. Ordinary opens preserve lockfile contents and mtimes.
    pub regenerate: bool,
}

impl DevelopmentOptions {
    #[must_use]
    pub fn new(context_dir: impl Into<PathBuf>, sdk_path: impl Into<PathBuf>) -> Self {
        Self { context_dir: context_dir.into(), sdk_path: sdk_path.into(), source_root: None, regenerate: false }
    }
}

/// A resolved development recipe. All filesystem paths are absolute and its
/// lockfile has been resolved before an editor starts using `--locked`.
#[derive(Debug, Clone)]
pub struct DevelopmentContext {
    pub source: BehaviorSource,
    pub manifest_path: PathBuf,
    pub sdk_path: PathBuf,
    /// Local dependencies (SDK, ABI, procedural macros) for read-only navigation.
    pub dependency_sources: Vec<PathBuf>,
    pub cargo: PathBuf,
    pub rustc: PathBuf,
    pub rustfmt: PathBuf,
    pub rustc_version: String,
    pub sysroot: PathBuf,
    pub sysroot_src: PathBuf,
    /// Explicit tool/recipe environment, including the GUI-launch PATH fallback.
    pub environment: BTreeMap<String, String>,
    pub target: &'static str,
}

impl DevelopmentContext {
    /// The canonical saved-source check / procedural-macro build command.
    /// Run it from the directory containing `manifest_path`.
    pub fn check_command(&self, target_dir: &Path) -> Result<Vec<String>> {
        Ok(vec![
            path_text(&self.cargo)?.to_owned(), "check".into(), "--locked".into(),
            "--release".into(), "--lib".into(), "--keep-going".into(),
            "--target".into(), self.target.into(), "--target-dir".into(),
            path_text(target_dir)?.to_owned(), "--message-format=json".into(),
        ])
    }
}

/// Materialise/repair the canonical Cargo manifest and module wrapper without
/// compiling WASM or copying author files. Broken Rust remains editable: only
/// Cargo metadata/dependency resolution is required here, not a successful check.
/// Call from a worker thread; discovering tools/resolving dependencies can block.
pub fn prepare_development(entry: &Path, opts: &DevelopmentOptions) -> Result<DevelopmentContext> {
    let recipe = Recipe::resolve(entry, opts.source_root.as_deref(), &opts.sdk_path, None)?;
    std::fs::create_dir_all(&opts.context_dir)?;
    let root = simplified(std::fs::canonicalize(&opts.context_dir)?);
    let directory = root.join("crates").join(path_segment(&recipe.workspace_key()));
    let _lock = lock_directory(&directory, "prepare.lock")?;
    if opts.regenerate {
        for name in ["Cargo.lock", "lock-seed.sha256"] {
            match std::fs::remove_file(directory.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    recipe.materialise(&directory, recipe.source.entry())?;
    let tools = compile::Toolchain::discover(&directory)?;
    let metadata = compile::cargo_metadata(&directory, &tools)?;

    let output = Command::new(&tools.rustc.program).current_dir(&directory)
        .env("PATH", compile::augmented_path()).args(["--print", "sysroot"]).output()
        .map_err(|e| BuildError::CargoSpawn(e.to_string()))?;
    if !output.status.success() {
        return Err(BuildError::CargoFailed(String::from_utf8_lossy(&output.stderr).into_owned()));
    }
    let sysroot = String::from_utf8(output.stdout).map_err(|e| BuildError::InvalidInput(e.to_string()))?;
    let sysroot = simplified(std::fs::canonicalize(sysroot.trim())?);
    let sysroot_src = sysroot.join("lib/rustlib/src/rust/library");
    if !sysroot_src.join("core/src/lib.rs").is_file() {
        return Err(BuildError::InvalidInput("Rust standard-library sources are missing. Install them with: rustup component add rust-src".into()));
    }
    let rustfmt = sysroot.join("bin").join(if cfg!(windows) { "rustfmt.exe" } else { "rustfmt" });
    let rustfmt = if rustfmt.is_file() { rustfmt } else {
        compile::find_program(std::ffi::OsStr::new("rustfmt"), &directory)
            .ok_or_else(|| BuildError::InvalidInput("Rust formatter is missing. Install it with: rustup component add rustfmt".into()))?
    };
    let mut environment = BTreeMap::new();
    for (key, value) in std::env::vars_os().filter(|(key, _)| compile::recipe_environment(key)) {
        let key = key.into_string().map_err(|_| BuildError::InvalidInput("non-UTF-8 toolchain environment key".into()))?;
        let value = value.into_string().map_err(|_| BuildError::InvalidInput(format!("non-UTF-8 value for {key}")))?;
        environment.insert(key, value);
    }
    environment.insert("PATH".into(), compile::augmented_path().into_string()
        .map_err(|_| BuildError::InvalidInput("non-UTF-8 toolchain PATH".into()))?);
    environment.insert("CARGO".into(), path_text(&tools.cargo.program)?.to_owned());
    environment.insert("RUSTC".into(), path_text(&tools.rustc.program)?.to_owned());
    // Pin rustup proxies even when VS Code was already running, or when an
    // author's source folder would select a different rust-toolchain.toml.
    environment.insert("RUSTUP_TOOLCHAIN".into(), path_text(&sysroot)?.to_owned());
    for name in ["CARGO_HOME", "RUSTUP_HOME"] {
        if let Some(value) = std::env::var_os(name) {
            let path = PathBuf::from(value);
            let path = if path.is_absolute() { path } else { directory.join(path) };
            environment.insert(name.into(), path_text(&path)?.to_owned());
        }
    }
    let mut dependency_sources = Vec::new();
    for package in metadata["packages"].as_array().into_iter().flatten() {
        if package["source"].is_null() && package["id"] != metadata["resolve"]["root"] {
            if let Some(path) = package["manifest_path"].as_str().and_then(|s| Path::new(s).parent()) {
                dependency_sources.push(simplified(path.to_path_buf()));
            }
        }
    }
    dependency_sources.sort();
    dependency_sources.dedup();
    Ok(DevelopmentContext {
        source: recipe.source, manifest_path: directory.join("Cargo.toml"), sdk_path: recipe.sdk,
        dependency_sources, cargo: tools.cargo.program, rustc: tools.rustc.program, rustfmt,
        rustc_version: tools.rustc.version.lines().next().unwrap_or("rustc").to_owned(),
        sysroot, sysroot_src, environment, target: compile::WASM_TARGET,
    })
}
