//! Per-target packaging: wrap a cooked *payload* into a launchable
//! bundle by co-locating the runtime binary and any platform scaffolding.
//!
//! The [`cook`](crate::cook) step is target-agnostic — it produces
//! `stage.usdz`, a `behaviors/` dir, and a stamped `run.toml` in one directory.
//! A [`Packager`] takes that [`CookReport`] and assembles the final artifact for
//! one [`CookTarget`]:
//!
//! * [`DesktopFolderPackager`] — a portable folder (`<name>/`) with the runtime
//!   binary, the payload, and any staged runtime libraries flat beside the
//!   binary. Works on any host and is what the Windows/Linux desktop targets
//!   ship. This is the library home for the logic `cargo xtask` and the
//!   Conductor "Package" command both drive, so bundling lives in one place.
//! * [`MacAppPackager`] — a macOS `.app` skeleton
//!   (`Contents/MacOS` + `Contents/Frameworks` + `Info.plist`). The payload and
//!   `run.toml` sit next to the binary in `Contents/MacOS/` so the runtime's
//!   `RuntimeManifest::discover()` (which looks for `run.toml` beside the
//!   executable) works unchanged. Codesigning / notarization is layered on top
//!   by a later step and is intentionally *not* done here.
//!
//! The packagers assemble the on-disk **layout** only. Producing the runtime
//! binary itself (`cargo build --target … --features …`) and staging the `OpenUSD`
//! dylibs remain the caller's job — passed in via [`RuntimeBinary`] — because
//! that is host-toolchain- and SDK-bound and cannot be exercised in every
//! environment.

use std::path::{Path, PathBuf};

use crate::Packager;
use crate::cook::{CookError, CookReport, CookTarget, PAYLOAD_MANIFEST};

/// The runtime binary (and optional runtime libraries) to co-locate with a
/// cooked payload.
#[derive(Debug, Clone)]
pub struct RuntimeBinary {
    /// Path to the already-built runtime executable for the target.
    pub executable: PathBuf,
    /// File name the executable should take inside the bundle (e.g.
    /// `"idtx-runtime.exe"` / `"MyExperience"`). Defaults to the source file
    /// name when `None`.
    pub bundled_name: Option<String>,
    /// Directory of runtime support libraries (`OpenUSD` dylibs, resolver plugin,
    /// `MaterialX` libs, the `usd/` plugin dir) to stage beside the binary. `None`
    /// leaves library staging to the caller / a later step.
    pub libs_dir: Option<PathBuf>,
    /// Android only: directory of `OpenUSD` plugInfo trees (`lib/usd` + `plugin/usd`)
    /// to stage into the APK `assets/usd/` so the on-device runtime can point
    /// `PXR_PLUGINPATH_NAME` at them. The monolithic `libusd_ms.so` carries the
    /// code; these are the discovery trees (`plugInfo.json` + schema resources).
    /// `None` on non-Android packagers (which ignore it).
    pub usd_plugins_dir: Option<PathBuf>,
    /// Standalone only: directory of the **cross-built** coded `OpenExec` plugin
    /// (`libidtx_usd_exec.{so,dylib}` + its `plugInfo.json` + `generatedSchema.usda`),
    /// staged beside the runtime by `cargo xtask build-{quest,visionos}-runtime
    /// --with-openexec`. The standalone packagers copy it into the bundle's
    /// `usd-exec/` (where the on-device runtime registers it via
    /// `idtx_usd_exec::install_at`). `None` when exec was not requested, or on the
    /// desktop packagers (whose host plugin comes from the cook, not here).
    pub exec_plugin_dir: Option<PathBuf>,
    /// Standalone only: directory of the **cross-built** HTTP USD resolver plugin
    /// (`libidtx_http_resolver.{so,dylib}` + its `plugInfo.json`), staged beside
    /// the runtime by `cargo xtask build-quest-runtime --with-remote`. The Android
    /// packager copies the `.so` into `jniLibs/arm64-v8a/` (it is a forced
    /// `DT_NEEDED` of the runtime under the `remote` feature) and the plugInfo into
    /// `assets/idtx-http-resolver/`. `None` when `remote` was not requested, or on
    /// packagers that handle the resolver elsewhere (the macOS `.app` bundler).
    pub resolver_plugin_dir: Option<PathBuf>,
}

impl RuntimeBinary {
    /// A binary with no bundled-name override and no staged libraries.
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            bundled_name: None,
            libs_dir: None,
            usd_plugins_dir: None,
            exec_plugin_dir: None,
            resolver_plugin_dir: None,
        }
    }

    /// The file name this binary takes inside the bundle.
    fn file_name(&self) -> Result<String, CookError> {
        if let Some(name) = &self.bundled_name {
            return Ok(name.clone());
        }
        self.executable
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .ok_or_else(|| CookError::MissingEntryStage(self.executable.clone()))
    }
}

// ── shared helpers ──────────────────────────────────────────────────────────

/// Copy a file, creating parent directories, mapping I/O errors to [`CookError`].
pub(crate) fn copy_file(from: &Path, to: &Path) -> Result<(), CookError> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|source| CookError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::copy(from, to).map_err(|source| CookError::Io {
        path: to.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Write `contents` to `to`, creating parent directories. Used by packagers that
/// materialize generated scaffolding (manifests, gradle files) rather than
/// copying an existing artifact.
pub(crate) fn write_file(to: &Path, contents: &str) -> Result<(), CookError> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|source| CookError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(to, contents).map_err(|source| CookError::Io {
        path: to.to_path_buf(),
        source,
    })
}

/// Recursively copy a directory tree.
pub(crate) fn copy_tree(from: &Path, to: &Path) -> Result<(), CookError> {
    std::fs::create_dir_all(to).map_err(|source| CookError::Io {
        path: to.to_path_buf(),
        source,
    })?;
    let entries = std::fs::read_dir(from).map_err(|source| CookError::Io {
        path: from.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| CookError::Io {
            path: from.to_path_buf(),
            source,
        })?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            copy_file(&src, &dst)?;
        }
    }
    Ok(())
}

/// Copy the cooked payload (everything in `report.out_dir`: `stage.usdz`,
/// `behaviors/`, `run.toml`) into `dest`.
fn stage_payload(report: &CookReport, dest: &Path, runtime: &RuntimeBinary, target: &CookTarget) -> Result<(), CookError> {
    let metadata = idtx_build_metadata::RuntimeBuildMetadata::read_binary(&runtime.executable)
        .map_err(|e| CookError::RuntimeMetadata(format!("{}: {e}", runtime.executable.display())))?;
    let mut manifest = idtx_runtime_manifest::RuntimeManifest::load(&report.manifest)
        .map_err(|e| CookError::RuntimeMetadata(e.to_string()))?;
    if metadata.target != target.triple {
        return Err(CookError::RuntimeMetadata(format!("runtime targets {}, requested {}", metadata.target, target.triple)));
    }
    if manifest.pins.abi_version.is_some_and(|abi| abi != metadata.abi_version)
        || manifest.pins.glam.as_ref().is_some_and(|glam| glam != &metadata.dependencies.glam) {
        return Err(CookError::RuntimeMetadata("runtime and cooked behavior ABI/glam pins disagree; rebuild with matching SDK/runtime".into()));
    }
    let mut required = manifest.pins.required_features.clone();
    if manifest.pins.runtime_target.is_none() { required.extend(manifest.pins.features.iter().cloned()); }
    required.extend(target.features.iter().cloned());
    required.sort();
    required.dedup();
    let missing: Vec<_> = required.iter().filter(|f| !metadata.features.contains(f)).collect();
    if !missing.is_empty() {
        return Err(CookError::RuntimeMetadata(format!("selected runtime lacks required features: {missing:?}")));
    }
    // The final package inventory includes these pins and the runtime bytes.
    manifest.stamp.content_hash = None;
    manifest.pins = idtx_runtime_manifest::PinsConfig {
        abi_version: Some(metadata.abi_version), glam: Some(metadata.dependencies.glam),
        wgpu: Some(metadata.dependencies.wgpu), wasmtime: Some(metadata.dependencies.wasmtime),
        features: metadata.features, required_features: required,
        runtime_target: Some(metadata.target), rustc: Some(metadata.rustc),
    };
    copy_tree(&report.out_dir, dest)?;
    write_file(&dest.join(PAYLOAD_MANIFEST), &manifest.to_toml_string())
}

/// Write `<assets>/payload.manifest`: one relative, forward-slash path per
/// payload file, so the on-device runtime can extract the read-only APK bundle
/// by exact path (the ndk `AssetManager` cannot enumerate subdirectories, so a
/// directory walk on-device is impossible — this manifest is the file list).
/// The manifest excludes itself. Call after the payload (and any USD plugin
/// trees) are staged into `assets/`.
fn write_payload_manifest(assets: &Path) -> Result<(), CookError> {
    fn collect(dir: &Path, root: &Path, out: &mut Vec<String>) -> Result<(), CookError> {
        let io = |path: &Path, source: std::io::Error| CookError::Io {
            path: path.to_path_buf(),
            source,
        };
        for entry in std::fs::read_dir(dir).map_err(|e| io(dir, e))? {
            let entry = entry.map_err(|e| io(dir, e))?;
            let path = entry.path();
            if entry.file_type().map_err(|e| io(&path, e))?.is_dir() {
                collect(&path, root, out)?;
            } else {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
        Ok(())
    }

    let mut rels = Vec::new();
    collect(assets, assets, &mut rels)?;
    rels.retain(|r| r != "payload.manifest");
    rels.sort();
    let text = format!("{}\n", rels.join("\n"));
    let manifest = assets.join("payload.manifest");
    std::fs::write(&manifest, text).map_err(|source| CookError::Io {
        path: manifest,
        source,
    })
}

/// Stage the runtime binary (as `bin_dir/<file_name>`) and, when present, its
/// libraries into `libs_dest`.
fn stage_runtime(
    runtime: &RuntimeBinary,
    bin_dir: &Path,
    libs_dest: &Path,
) -> Result<PathBuf, CookError> {
    if !runtime.executable.is_file() {
        return Err(CookError::MissingEntryStage(runtime.executable.clone()));
    }
    let bin_path = bin_dir.join(runtime.file_name()?);
    copy_file(&runtime.executable, &bin_path)?;
    if let Some(libs) = &runtime.libs_dir
        && libs.is_dir() {
            copy_tree(libs, libs_dest)?;
        }
    Ok(bin_path)
}

// ── desktop folder packager (Windows / Linux) ───────────────────────────────

/// Packages a cooked payload into a portable folder: the runtime binary,
/// `run.toml`, `stage.usdz`, `behaviors/`, and any staged runtime libraries all
/// flat inside `<out_root>/<name>/`. The runtime finds `run.toml` beside the
/// binary via `RuntimeManifest::discover()`.
#[derive(Debug, Clone)]
pub struct DesktopFolderPackager {
    target: CookTarget,
    runtime: RuntimeBinary,
    out_root: PathBuf,
    /// Bundle folder name (defaults to the runtime binary's stem).
    bundle_name: Option<String>,
}

impl DesktopFolderPackager {
    /// A packager writing `<out_root>/<name>/`.
    #[must_use]
    pub fn new(target: CookTarget, runtime: RuntimeBinary, out_root: impl Into<PathBuf>) -> Self {
        Self {
            target,
            runtime,
            out_root: out_root.into(),
            bundle_name: None,
        }
    }

    /// Override the bundle folder name.
    #[must_use]
    pub fn with_bundle_name(mut self, name: impl Into<String>) -> Self {
        self.bundle_name = Some(name.into());
        self
    }

    fn resolved_name(&self) -> Result<String, CookError> {
        if let Some(n) = &self.bundle_name {
            return Ok(n.clone());
        }
        self.runtime
            .executable
            .file_stem()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .ok_or_else(|| CookError::MissingEntryStage(self.runtime.executable.clone()))
    }
}

impl Packager for DesktopFolderPackager {
    fn target(&self) -> &CookTarget {
        &self.target
    }

    fn package(&self, report: &CookReport) -> Result<PathBuf, CookError> {
        self.package_with(report, |_| Ok(()))
    }
}

impl DesktopFolderPackager {
    /// Add host-discovered resources before hashing and publishing the bundle.
    ///
    /// # Errors
    /// Returns any staging, finalization or publication error, preserving the previous bundle.
    pub fn package_with(&self, report: &CookReport, finalize: impl FnOnce(&Path) -> Result<(), CookError>) -> Result<PathBuf, CookError> {
        let publication = crate::publication::Publication::new(&self.out_root.join(self.resolved_name()?))?;
        let bundle = publication.path();
        // Payload + binary + libs are all flat in the bundle root.
        stage_payload(report, &bundle, &self.runtime, &self.target)?;
        stage_runtime(&self.runtime, &bundle, &bundle)?;
        finalize(&bundle)?;
        crate::publication::stamp(&bundle, &bundle.join(PAYLOAD_MANIFEST))?;
        publication.publish()
    }
}

// ── macOS .app packager ─────────────────────────────────────────────────────

/// Packages a cooked payload into a macOS `.app` skeleton. The payload and
/// `run.toml` live in `Contents/MacOS/` beside the binary (so
/// `RuntimeManifest::discover()` works unchanged); runtime libraries go in
/// `Contents/Frameworks/`. An `Info.plist` is emitted from [`MacAppInfo`].
///
/// Codesigning / notarization are **not** performed here.
#[derive(Debug, Clone)]
pub struct MacAppPackager {
    target: CookTarget,
    runtime: RuntimeBinary,
    out_root: PathBuf,
    info: MacAppInfo,
}

/// The subset of `Info.plist` fields the packager emits.
#[derive(Debug, Clone)]
pub struct MacAppInfo {
    /// `CFBundleName` — the app display name (also the `.app` folder stem).
    pub name: String,
    /// `CFBundleIdentifier` — reverse-DNS bundle id (e.g. `com.example.exp`).
    pub identifier: String,
    /// `CFBundleExecutable` — the binary name inside `Contents/MacOS/`.
    pub executable: String,
}

impl MacAppInfo {
    /// Render the minimal `Info.plist` XML for this bundle.
    #[must_use]
    pub fn to_plist(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
\t<key>CFBundleName</key><string>{name}</string>\n\
\t<key>CFBundleIdentifier</key><string>{id}</string>\n\
\t<key>CFBundleExecutable</key><string>{exe}</string>\n\
\t<key>CFBundlePackageType</key><string>APPL</string>\n\
\t<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>\n\
\t<key>NSHighResolutionCapable</key><true/>\n\
</dict>\n\
</plist>\n",
            name = self.name,
            id = self.identifier,
            exe = self.executable,
        )
    }
}

impl MacAppPackager {
    /// A packager writing `<out_root>/<info.name>.app`.
    #[must_use]
    pub fn new(
        target: CookTarget,
        runtime: RuntimeBinary,
        out_root: impl Into<PathBuf>,
        info: MacAppInfo,
    ) -> Self {
        Self {
            target,
            runtime,
            out_root: out_root.into(),
            info,
        }
    }
}

impl Packager for MacAppPackager {
    fn target(&self) -> &CookTarget {
        &self.target
    }

    fn package(&self, report: &CookReport) -> Result<PathBuf, CookError> {
        self.package_with(report, |_| Ok(()))
    }
}

impl MacAppPackager {
    /// Add host-discovered resources before hashing and publishing the app.
    ///
    /// # Errors
    /// Returns any staging, finalization or publication error, preserving the previous app.
    pub fn package_with(&self, report: &CookReport, finalize: impl FnOnce(&Path) -> Result<(), CookError>) -> Result<PathBuf, CookError> {
        let publication = crate::publication::Publication::new(&self.out_root.join(format!("{}.app", self.info.name)))?;
        let app = publication.path();
        let contents = app.join("Contents");
        let macos = contents.join("MacOS");
        let frameworks = contents.join("Frameworks");

        // Payload + run.toml live beside the binary in Contents/MacOS/.
        stage_payload(report, &macos, &self.runtime, &self.target)?;
        // Force the binary name to match CFBundleExecutable.
        let runtime = RuntimeBinary {
            bundled_name: Some(self.info.executable.clone()),
            ..self.runtime.clone()
        };
        stage_runtime(&runtime, &macos, &frameworks)?;

        // Info.plist
        let plist = contents.join("Info.plist");
        std::fs::write(&plist, self.info.to_plist()).map_err(|source| CookError::Io {
            path: plist.clone(),
            source,
        })?;

        finalize(&app)?;
        crate::publication::stamp(&app, &macos.join(PAYLOAD_MANIFEST))?;
        publication.publish()
    }
}

// ── Android .apk packager (Quest standalone) ─────────────────────────────────

/// The Android native-library ABI subdirectory under `jniLibs/`. Quest is
/// 64-bit ARM, so the only ABI staged is `arm64-v8a`.
pub const ANDROID_ABI_DIR: &str = "arm64-v8a";

/// The runtime cdylib stem the `AndroidManifest.xml` `android.app.lib_name`
/// meta-data points `NativeActivity` at (the on-device file is
/// `lib<stem>.so`). Must match the `[lib] name` of the `aarch64-linux-android`
/// runtime build.
pub const ANDROID_RUNTIME_LIB_NAME: &str = "idtx_runtime";

/// ELF `e_machine` for `AArch64` (`EM_AARCH64`), the only ABI Quest ships.
const EM_AARCH64: u16 = 0xB7;

/// Verify `path` is an ELF shared object/executable for `expected_machine`
/// (ELF `e_machine`), reading only the file header.
///
/// This fails closed so a host binary can never be staged into an Android
/// `jniLibs/<abi>/` dir — the field regression that stuffed a Windows PE
/// (`idtx-runtime.exe`, `MZ` magic) into the Quest APK, which the on-device
/// loader could never load. A wrong-architecture ELF (e.g. an x86-64 host
/// `.so`) is rejected too, so only a real aarch64 object reaches the APK.
fn verify_elf_machine(path: &Path, expected: &str, expected_machine: u16) -> Result<(), CookError> {
    use std::io::Read as _;
    let mut header = [0u8; 20];
    let mut file = std::fs::File::open(path).map_err(|source| CookError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    // A short read (file smaller than an ELF header) is itself a rejection.
    file.read_exact(&mut header)
        .map_err(|_| CookError::InvalidRuntimeBinary {
            path: path.to_path_buf(),
            expected: expected.to_owned(),
            detail: "file is too small to be an ELF object".to_owned(),
        })?;
    if &header[0..4] != b"\x7fELF" {
        let detail = if &header[0..2] == b"MZ" {
            "found a Windows PE/COFF binary (MZ) — a host .exe cannot run on Android".to_owned()
        } else {
            format!("not an ELF object (magic {:02x?})", &header[0..4])
        };
        return Err(CookError::InvalidRuntimeBinary {
            path: path.to_path_buf(),
            expected: expected.to_owned(),
            detail,
        });
    }
    // e_machine is a little-endian u16 at offset 18 (ELF64, little-endian — the
    // only shape the Android/arm64 toolchain emits).
    let machine = u16::from_le_bytes([header[18], header[19]]);
    if machine != expected_machine {
        return Err(CookError::InvalidRuntimeBinary {
            path: path.to_path_buf(),
            expected: expected.to_owned(),
            detail: format!("wrong architecture (ELF e_machine 0x{machine:04x})"),
        });
    }
    Ok(())
}

/// Verify `path` is a thin **Mach-O arm64** object, reading only the file header.
///
/// The visionOS analog of [`verify_elf_machine`]: it fails closed so a host
/// binary (an `x86_64` Mach-O, a Windows PE, an Android ELF) can never be staged
/// into a Vision Pro `.app` `Frameworks/`. Checks the 64-bit little-endian magic
/// `MH_MAGIC_64` (`0xFEEDFACF`) and the `CPU_TYPE_ARM64` cputype (`0x0100000C`);
/// a fat/universal binary (`0xCAFEBABE`) is rejected too, as the `.app` embeds a
/// thin arm64 slice.
fn verify_macho_arm64(path: &Path, expected: &str) -> Result<(), CookError> {
    use std::io::Read as _;
    let invalid = |detail: String| CookError::InvalidRuntimeBinary {
        path: path.to_path_buf(),
        expected: expected.to_owned(),
        detail,
    };
    let mut header = [0u8; 8];
    let mut file = std::fs::File::open(path).map_err(|source| CookError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    file.read_exact(&mut header)
        .map_err(|_| invalid("file is too small to be a Mach-O object".to_owned()))?;
    if header[0..4] != [0xCF, 0xFA, 0xED, 0xFE] {
        let detail = if header[0..2] == [b'M', b'Z'] {
            "found a Windows PE/COFF binary (MZ) — a host .exe cannot run on visionOS".to_owned()
        } else if header[0..4] == [0x7F, b'E', b'L', b'F'] {
            "found an ELF object (Linux/Android) — not an Apple Mach-O".to_owned()
        } else if header[0..4] == [0xCA, 0xFE, 0xBA, 0xBE] {
            "found a fat/universal Mach-O — the .app embeds a thin arm64 slice".to_owned()
        } else {
            format!("not a Mach-O object (magic {:02x?})", &header[0..4])
        };
        return Err(invalid(detail));
    }
    // cputype is a little-endian u32 at offset 4 (Mach-O 64-bit, little-endian —
    // the only shape the Apple arm64 toolchain emits).
    let cputype = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    if cputype != 0x0100_000C {
        return Err(invalid(format!(
            "wrong architecture (Mach-O cputype 0x{cputype:08x})"
        )));
    }
    Ok(())
}

/// Embedded gradle-project + manifest templates, substituted at package time.
const ANDROID_MANIFEST_TEMPLATE: &str = include_str!("../templates/android/AndroidManifest.xml");
const ANDROID_BUILD_GRADLE_TEMPLATE: &str = include_str!("../templates/android/build.gradle");
const ANDROID_SETTINGS_GRADLE_TEMPLATE: &str = include_str!("../templates/android/settings.gradle");

/// Materialize the minimal gradle project (`AndroidManifest.xml`,
/// `build.gradle`, `settings.gradle`) into `staging` from the embedded
/// templates, substituting the application id and display label. The staging
/// directory is the gradle module root; `jniLibs/` and `assets/` are staged
/// beside these by the packager.
fn write_android_project(
    staging: &Path,
    package_id: &str,
    label: &str,
    lib_name: &str,
) -> Result<(), CookError> {
    let manifest = ANDROID_MANIFEST_TEMPLATE
        .replace("{{PACKAGE}}", package_id)
        .replace("{{LABEL}}", label)
        .replace("{{LIB_NAME}}", lib_name);
    write_file(&staging.join("AndroidManifest.xml"), &manifest)?;

    let build_gradle = ANDROID_BUILD_GRADLE_TEMPLATE.replace("{{PACKAGE}}", package_id);
    write_file(&staging.join("build.gradle"), &build_gradle)?;

    let settings = ANDROID_SETTINGS_GRADLE_TEMPLATE.replace("{{LABEL}}", label);
    write_file(&staging.join("settings.gradle"), &settings)?;
    Ok(())
}

/// Reduce an arbitrary bundle name to a reverse-DNS-safe `applicationId`
/// (`com.idtx.<slug>`). Non-alphanumerics collapse to nothing so the result is
/// a single valid package segment.
fn android_package_id(name: &str) -> String {
    let slug: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if slug.is_empty() {
        "com.idtx.experience".to_owned()
    } else {
        format!("com.idtx.{slug}")
    }
}

/// Stage the cross-built coded `OpenExec` plugin for an Android APK: its native
/// `.so` into `jni_libs` (the APK native-lib dir, so it loads in the app linker
/// namespace alongside `libusd_ms.so`), and its `plugInfo.json` (with
/// `LibraryPath` emptied) + `generatedSchema.usda` into `assets/<exec-dir>/` for
/// `PlugRegistry` discovery. See the call site for why the `.so` must not go to
/// the data dir. Non-`.so`, non-resource files ride along in assets.
fn stage_exec_plugin_android(
    exec_dir: &Path,
    jni_libs: &Path,
    assets: &Path,
) -> Result<(), CookError> {
    let res_dest = assets.join(idtx_standalone::EXEC_PLUGIN_DIR_NAME);
    for entry in std::fs::read_dir(exec_dir).map_err(|source| CookError::Io {
        path: exec_dir.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| CookError::Io {
            path: exec_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with(".so") {
            // Native code → APK native-lib dir (app namespace / global group).
            copy_file(&path, &jni_libs.join(&name))?;
        } else if name_str == "plugInfo.json" {
            // Resource → assets, but neuter LibraryPath: the code is loaded by
            // soname from the native-lib dir, not by PlugRegistry from the data dir.
            let text = std::fs::read_to_string(&path).map_err(|source| CookError::Io {
                path: path.clone(),
                source,
            })?;
            write_file(&res_dest.join("plugInfo.json"), &empty_plug_info_library_path(&text))?;
        } else {
            // generatedSchema.usda and any other resources ride along in assets.
            copy_file(&path, &res_dest.join(&name))?;
        }
    }
    Ok(())
}

/// Stage the cross-built HTTP USD resolver plugin for an Android APK: its native
/// `.so` into `jni_libs` (the APK native-lib dir, so the forced `DT_NEEDED
/// libidtx_http_resolver.so` resolves at launch and the plugin loads in the app
/// linker namespace alongside `libusd_ms.so`), and its `plugInfo.json` (with
/// `LibraryPath` emptied) into `assets/idtx-http-resolver/` for `PlugRegistry`
/// discovery. Mirrors [`stage_exec_plugin_android`]; the resolver is both a
/// link-time dependency *and* a `PlugRegistry` plugin, so — exactly like `OpenExec` —
/// the code must live in the native-lib dir (loaded by soname) while only the
/// resource rides along in the payload with its `LibraryPath` neutered.
fn stage_resolver_plugin_android(
    resolver_dir: &Path,
    jni_libs: &Path,
    assets: &Path,
) -> Result<(), CookError> {
    let res_dest = assets.join(idtx_standalone::HTTP_RESOLVER_PLUGIN_DIR_NAME);
    for entry in std::fs::read_dir(resolver_dir).map_err(|source| CookError::Io {
        path: resolver_dir.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| CookError::Io {
            path: resolver_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with(".so") {
            // Native code → APK native-lib dir (app namespace / global group).
            copy_file(&path, &jni_libs.join(&name))?;
        } else if name_str == "plugInfo.json" {
            // Resource → assets, but neuter LibraryPath: the code is loaded by
            // soname from the native-lib dir, not by PlugRegistry from the data dir.
            let text = std::fs::read_to_string(&path).map_err(|source| CookError::Io {
                path: path.clone(),
                source,
            })?;
            write_file(&res_dest.join("plugInfo.json"), &empty_plug_info_library_path(&text))?;
        } else {
            // Any other resource rides along in assets.
            copy_file(&path, &res_dest.join(&name))?;
        }
    }
    Ok(())
}


/// `libusd_ms.dylib`), and its `plugInfo.json` (with `LibraryPath` emptied) +
/// `generatedSchema.usda` into `<app>/<exec-dir>/` for `PlugRegistry` discovery
/// via the immersive entrypoint's `install_at`.
///
/// Why the `.dylib` must live in `Frameworks/`, not `usd-exec/`: the runtime
/// cdylib's build script (`apps/idtx-runtime/build.rs`) forces a hard
/// `LC_LOAD_DYLIB` on `@rpath/libidtx_usd_exec.dylib` for the visionOS target (so
/// the plugin's and the core's weak C++ `type_info` collapse to one copy in the
/// same linker image — Apple libc++ compares `type_info` by pointer, so a late
/// `dlopen` would split them and `OpenExec` aborts). dyld resolves that
/// `@rpath/...` dependency **at launch**, before any `install_at` runs, using the
/// bundle's rpaths — which point only at `Frameworks/`. Staging the `.dylib` in
/// `usd-exec/` (not an rpath) makes the app fail to load before startup. This
/// mirrors the Android packager, which puts the `.so` in the native-lib dir and
/// keeps only the (emptied) plugInfo in the payload.
fn stage_exec_plugin_visionos(
    exec_dir: &Path,
    frameworks: &Path,
    app: &Path,
) -> Result<(), CookError> {
    let res_dest = app.join(idtx_standalone::EXEC_PLUGIN_DIR_NAME);
    for entry in std::fs::read_dir(exec_dir).map_err(|source| CookError::Io {
        path: exec_dir.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| CookError::Io {
            path: exec_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with(".dylib") {
            // Native code → Frameworks/ (resolves the forced `@rpath/...` load).
            copy_file(&path, &frameworks.join(&name))?;
        } else if name_str == "plugInfo.json" {
            // Resource → usd-exec/, but neuter LibraryPath: the code is already
            // resident via the `LC_LOAD_DYLIB` from Frameworks/, so `PlugRegistry`
            // must register the schema/compute metadata without re-`dlopen`ing it.
            let text = std::fs::read_to_string(&path).map_err(|source| CookError::Io {
                path: path.clone(),
                source,
            })?;
            write_file(&res_dest.join("plugInfo.json"), &empty_plug_info_library_path(&text))?;
        } else {
            // generatedSchema.usda and any other resources ride along in usd-exec/.
            copy_file(&path, &res_dest.join(&name))?;
        }
    }
    Ok(())
}

/// Rewrite a `plugInfo.json`'s `"LibraryPath": "<x>"` value to the empty string, so
/// `PlugRegistry` treats the plugin as "code already resident" and does not
/// `dlopen` a library (the `.so`/`.dylib` is loaded by soname / `LC_LOAD_DYLIB`
/// from the native-lib dir / `Frameworks/` instead). Mirrors the empty
/// `LibraryPath` a Godot-bundled USD extension ships.
/// A plugInfo without a `LibraryPath` key is returned unchanged.
///
/// Public so the desktop resolver staging (`idtx-studio-shell`'s
/// `stage_desktop_http_resolver`) and the Windows/visionOS bundlers share one
/// implementation rather than each carrying a divergent copy.
#[must_use]
pub fn empty_plug_info_library_path(plug_info: &str) -> String {
    let Some(key_start) = plug_info.find("\"LibraryPath\"") else {
        return plug_info.to_owned();
    };
    // Find the value string: the first '"' after the ':' following the key.
    let after_key = &plug_info[key_start..];
    let Some(colon) = after_key.find(':') else {
        return plug_info.to_owned();
    };
    let rest = &after_key[colon + 1..];
    let Some(open_q) = rest.find('"') else {
        return plug_info.to_owned();
    };
    let Some(close_rel) = rest[open_q + 1..].find('"') else {
        return plug_info.to_owned();
    };
    // Absolute offsets of the opening/closing quotes in the original string.
    let abs_open = key_start + colon + 1 + open_q;
    let abs_close = abs_open + 1 + close_rel;
    let mut out = String::with_capacity(plug_info.len());
    out.push_str(&plug_info[..=abs_open]); // up to and including the opening quote
    out.push_str(&plug_info[abs_close..]); // from the closing quote on (value emptied)
    out
}

/// Does the runtime binary carry a hard load command on the coded `OpenExec` plugin
/// (`libidtx_usd_exec.{dylib,so}`)? `apps/idtx-runtime/build.rs` forces that
/// `LC_LOAD_DYLIB` / `DT_NEEDED` only under the `openexec` feature, and dyld / the
/// Android loader resolves it **at launch** — so when it is present the plugin
/// library MUST be staged beside the runtime or the packaged app crashes before
/// `main`.
///
/// Detected by scanning the Mach-O / ELF bytes for the literal dependency name:
/// a load command / `DT_NEEDED` records the library name as a NUL-terminated
/// string in the file, so the exact stem `libidtx_usd_exec.{dylib,so}` appears
/// verbatim. This is dependency-free and has no realistic false positives — that
/// stem does not occur incidentally. A read failure conservatively returns
/// `false` (the format guards elsewhere already reject unreadable runtimes).
fn runtime_forces_exec_load(runtime: &Path) -> bool {
    const STEMS: &[&[u8]] = &[b"libidtx_usd_exec.dylib", b"libidtx_usd_exec.so"];
    let Ok(bytes) = std::fs::read(runtime) else {
        return false;
    };
    STEMS
        .iter()
        .any(|stem| bytes.windows(stem.len()).any(|w| w == *stem))
}

/// Does the runtime binary carry a hard load command on the HTTP resolver plugin
/// (`libidtx_http_resolver.{dylib,so}`)? The `remote` / `usd-resolver-http`
/// feature links the resolver as a forced `DT_NEEDED` / `LC_LOAD_DYLIB`, resolved
/// by the loader **at launch** — so when present the resolver library MUST be
/// staged beside the runtime or the packaged app crashes before `main`. Detected
/// by scanning for the literal NUL-terminated dependency name, like
/// [`runtime_forces_exec_load`]; a read failure conservatively returns `false`.
fn runtime_forces_resolver_load(runtime: &Path) -> bool {
    const STEMS: &[&[u8]] = &[b"libidtx_http_resolver.dylib", b"libidtx_http_resolver.so"];
    let Ok(bytes) = std::fs::read(runtime) else {
        return false;
    };
    STEMS
        .iter()
        .any(|stem| bytes.windows(stem.len()).any(|w| w == *stem))
}

/// Packages a cooked payload into an Android (Quest) `.apk` **staging directory**.
///
/// Layout under `<out_root>/<name>/`:
/// * `jniLibs/arm64-v8a/` — the runtime `cdylib` (`.so`) and every staged
///   native library (`libusd_ms.so`, `libtbb.so`, `libMaterialX*.so`,
///   `libc++_shared.so`), flat, where the Android loader expects them.
/// * `assets/` — the cooked payload (`stage.usdz` + `behaviors/` + `run.toml`)
///   **and** the USD plugin-discovery trees. The APK's `assets/` is the only
///   place the on-device `PluginExtraction` step (see `idtx_standalone`) can
///   reach to copy the plugInfo trees into a writable dir, so the payload is
///   bundled here rather than beside a binary.
///
/// Real `.apk` assembly + signing (gradle / apksigner) runs under `xtask`/CI,
/// not in-editor.
#[derive(Debug, Clone)]
pub struct AndroidApkPackager {
    target: CookTarget,
    runtime: RuntimeBinary,
    out_root: PathBuf,
    /// Staging folder name (defaults to the runtime binary's stem).
    bundle_name: Option<String>,
    /// Android `applicationId` (defaults to `com.idtx.<slug of staging name>`).
    package_id: Option<String>,
}

impl AndroidApkPackager {
    /// A packager writing the `.apk` staging tree to `<out_root>/<name>/`.
    #[must_use]
    pub fn new(target: CookTarget, runtime: RuntimeBinary, out_root: impl Into<PathBuf>) -> Self {
        Self {
            target,
            runtime,
            out_root: out_root.into(),
            bundle_name: None,
            package_id: None,
        }
    }

    /// Override the staging folder name.
    #[must_use]
    pub fn with_bundle_name(mut self, name: impl Into<String>) -> Self {
        self.bundle_name = Some(name.into());
        self
    }

    /// Override the Android `applicationId`. When unset it is derived from the
    /// staging name as `com.idtx.<slug>`.
    #[must_use]
    pub fn with_package_id(mut self, id: impl Into<String>) -> Self {
        self.package_id = Some(id.into());
        self
    }

    fn resolved_name(&self) -> Result<String, CookError> {
        if let Some(n) = &self.bundle_name {
            return Ok(n.clone());
        }
        self.runtime
            .executable
            .file_stem()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .ok_or_else(|| CookError::MissingEntryStage(self.runtime.executable.clone()))
    }

    fn resolved_package_id(&self, name: &str) -> String {
        self.package_id
            .clone()
            .unwrap_or_else(|| android_package_id(name))
    }
}

impl Packager for AndroidApkPackager {
    fn target(&self) -> &CookTarget {
        &self.target
    }

    fn package(&self, report: &CookReport) -> Result<PathBuf, CookError> {
        let name = self.resolved_name()?;
        let package_id = self.resolved_package_id(&name);
        let publication = crate::publication::Publication::new(&self.out_root.join(&name))?;
        let staging = publication.path();
        let jni_libs = staging.join("jniLibs").join(ANDROID_ABI_DIR);
        let assets = staging.join("assets");

        // Fail closed on a wrong-format/arch runtime: the input MUST be a real
        // aarch64 ELF shared object (`libidtx_runtime.so`), never a host PE
        // `.exe`. This is the guard against the field regression that staged a
        // Windows `idtx-runtime.exe` into `jniLibs/arm64-v8a/`.
        verify_elf_machine(
            &self.runtime.executable,
            "aarch64 ELF shared object (libidtx_runtime.so)",
            EM_AARCH64,
        )?;

        // Payload (stage + behaviors + run.toml) + the USD plugin trees live in
        // assets/, the only APK location the runtime extractor can reach.
        stage_payload(report, &assets, &self.runtime, &self.target)?;
        // OpenUSD plugInfo discovery trees → assets/usd/ (lib/usd + plugin/usd).
        // The on-device runtime extracts them and points PXR_PLUGINPATH_NAME at
        // them; without them the monolithic libusd_ms.so cannot find
        // ArDefaultResolver (its build-host default plugin paths do not exist on
        // device) and USD aborts at first stage open.
        if let Some(usd_plugins) = &self.runtime.usd_plugins_dir
            && usd_plugins.is_dir() {
                copy_tree(usd_plugins, &assets.join("usd"))?;
            }
        // Cross-built coded OpenExec plugin → assets/usd-exec/ (part of the
        // payload, so on-device extraction lands it at `<data>/payload/usd-exec/`
        // where the runtime's `staged_exec_dir` registers it). Staged before the
        // manifest so it is listed and extracted.
        // Cross-built coded OpenExec plugin. CRITICAL (Android linking): the
        // plugin's native `.so` must live in the APK native-lib dir
        // (`jniLibs/arm64-v8a/`, beside `libusd_ms.so`), NOT be extracted to the
        // app data dir. A `.so` loaded from a data-dir path is outside the app
        // linker namespace's global group, so its weak C++ `std::string`/`VtValue`
        // type symbols do not collapse onto `libusd_ms.so`'s — the plugin and the
        // core then disagree on type identity and OpenExec aborts
        // (`vectorAccessor.h`: "holding string, tried to use as string"). Placing
        // the `.so` in the native-lib dir (and loading it by soname at runtime)
        // puts it in the same namespace/global group as the core, so the types
        // unify — exactly how a Godot-bundled USD extension `.so` works on device.
        // Only the plugInfo + generatedSchema go into the payload (assets), with
        // `LibraryPath` emptied so `PlugRegistry` registers the schema/compute
        // metadata without re-loading the code (already resident via the soname load).
        if let Some(exec_dir) = &self.runtime.exec_plugin_dir
            && exec_dir.is_dir() {
                stage_exec_plugin_android(exec_dir, &jni_libs, &assets)?;
            }

        // Fail closed: if the runtime forces the OpenExec load but the plugin `.so`
        // is not actually staged, the Android loader would fail at launch. Catch it
        // here, not on device. The staging dir is reused across cooks, so a copied
        // destination can be stale — require the *current* source plugin to exist
        // (exec dir present and holding the lib) as well as the copied destination,
        // so an old `.so` left over from a previous cook cannot mask a runtime whose
        // exec dir is now absent / empty / missing the lib.
        if runtime_forces_exec_load(&self.runtime.executable) {
            let source_present = self
                .runtime
                .exec_plugin_dir
                .as_ref()
                .is_some_and(|dir| dir.join("libidtx_usd_exec.so").is_file());
            if !source_present || !jni_libs.join("libidtx_usd_exec.so").is_file() {
                return Err(CookError::MissingExecPlugin {
                    runtime: self.runtime.executable.clone(),
                    plugin: "libidtx_usd_exec.so".to_owned(),
                });
            }
        }

        // Cross-built HTTP resolver plugin (feature `remote`). Same Android-linking
        // reasoning as OpenExec above: the `.so` goes into `jniLibs/arm64-v8a/`
        // (resolving the forced `DT_NEEDED libidtx_http_resolver.so` and keeping the
        // resolver in the core's linker namespace), and only the (LibraryPath-emptied)
        // plugInfo rides along in `assets/idtx-http-resolver/`.
        if let Some(resolver_dir) = &self.runtime.resolver_plugin_dir
            && resolver_dir.is_dir() {
                stage_resolver_plugin_android(resolver_dir, &jni_libs, &assets)?;
            }

        // Fail closed: if the runtime forces the resolver load but its `.so` is not
        // actually staged, the Android loader fails at launch with
        // `UnsatisfiedLinkError`. Catch it here, not on device. As with the exec
        // guard, require the *current* source plugin to exist as well as the copied
        // destination, so a stale `.so` from a previous cook cannot mask a runtime
        // whose resolver dir is now absent / empty / missing the lib.
        if runtime_forces_resolver_load(&self.runtime.executable) {
            let source_present = self
                .runtime
                .resolver_plugin_dir
                .as_ref()
                .is_some_and(|dir| dir.join("libidtx_http_resolver.so").is_file());
            if !source_present || !jni_libs.join("libidtx_http_resolver.so").is_file() {
                return Err(CookError::MissingResolverPlugin {
                    runtime: self.runtime.executable.clone(),
                    plugin: "libidtx_http_resolver.so".to_owned(),
                });
            }
        }
        // Emit the file manifest the on-device runtime reads to extract the
        // read-only payload out to its writable data dir (the ndk AssetManager
        // cannot list subdirectories, so it extracts each listed path directly).
        write_payload_manifest(&assets)?;
        // Runtime cdylib + native libs together under jniLibs/arm64-v8a/.
        stage_runtime(&self.runtime, &jni_libs, &jni_libs)?;
        // Minimal gradle project (manifest + build/settings.gradle) at the root.
        write_android_project(&staging, &package_id, &name, ANDROID_RUNTIME_LIB_NAME)?;

        crate::publication::stamp(&staging, &assets.join(PAYLOAD_MANIFEST))?;
        publication.publish()
    }
}

// ── visionOS .app packager (Apple Vision Pro standalone) ─────────────────────

/// Packages a cooked payload into a visionOS `.app` **staging directory** (the
/// input to a later Xcode build + codesign). It lays out the on-disk
/// structure only; it does not invoke Xcode.
///
/// Layout under `<out_root>/<name>.app/`:
/// * `Contents/` — the runtime binary with the cooked payload (`stage.usdz` +
///   `behaviors/` + `run.toml`) beside it, so `RuntimeManifest::discover()`
///   finds `run.toml` next to the executable.
/// * `Contents/Frameworks/` — the embedded USD dylibs / `.xcframework` slices.
///
/// Codesign / notarize + the `Info.plist` immersive-space keys are not wired
/// here; they run on macOS only.
#[derive(Debug, Clone)]
pub struct VisionOsAppPackager {
    target: CookTarget,
    runtime: RuntimeBinary,
    out_root: PathBuf,
    /// Bundle name (the `.app` stem); defaults to the runtime binary's stem.
    bundle_name: Option<String>,
    /// Explicit `CFBundleIdentifier` (must match the signing profile on device);
    /// `None` derives `com.sap.apeirora.idtx.<name>`.
    bundle_id: Option<String>,
    /// Add the ARKit hand-tracking usage string to the Info.plist.
    #[allow(clippy::doc_markdown)]
    hand_tracking: bool,
}

impl VisionOsAppPackager {
    /// A packager writing `<out_root>/<name>.app/`.
    #[must_use]
    pub fn new(target: CookTarget, runtime: RuntimeBinary, out_root: impl Into<PathBuf>) -> Self {
        Self {
            target,
            runtime,
            out_root: out_root.into(),
            bundle_name: None,
            bundle_id: None,
            hand_tracking: false,
        }
    }

    /// Override the bundle name (the `.app` stem).
    #[must_use]
    pub fn with_bundle_name(mut self, name: impl Into<String>) -> Self {
        self.bundle_name = Some(name.into());
        self
    }

    /// Set an explicit `CFBundleIdentifier` (device signing requires it to match
    /// the provisioning profile).
    #[must_use]
    pub fn with_bundle_id(mut self, bundle_id: impl Into<String>) -> Self {
        self.bundle_id = Some(bundle_id.into());
        self
    }

    /// Add the ARKit hand-tracking usage description to the Info.plist.
    #[must_use]
    #[allow(clippy::doc_markdown)]
    pub fn with_hand_tracking(mut self, enabled: bool) -> Self {
        self.hand_tracking = enabled;
        self
    }

    fn resolved_name(&self) -> Result<String, CookError> {
        if let Some(n) = &self.bundle_name {
            return Ok(n.clone());
        }
        self.runtime
            .executable
            .file_stem()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .ok_or_else(|| CookError::MissingEntryStage(self.runtime.executable.clone()))
    }
}

impl Packager for VisionOsAppPackager {
    fn target(&self) -> &CookTarget {
        &self.target
    }

    fn package(&self, report: &CookReport) -> Result<PathBuf, CookError> {
        let name = self.resolved_name()?;
        let publication = crate::publication::Publication::new(&self.out_root.join(format!("{name}.app")))?;
        let app = publication.path();
        // visionOS `.app`s are FLAT (iOS-style): the executable, Info.plist, and
        // Frameworks/ sit at the bundle root — NOT in a macOS-style Contents/.
        let frameworks = app.join("Frameworks");

        // Fail closed on a wrong-format/arch runtime: the input MUST be a real
        // Mach-O arm64 dynamic library (`libidtx_runtime.dylib`), never a host
        // x86_64 Mach-O / PE / Android ELF. The visionOS twin of the Android
        // packager's ELF guard.
        verify_macho_arm64(
            &self.runtime.executable,
            "Mach-O arm64 dynamic library (libidtx_runtime.dylib)",
        )?;

        // Payload + run.toml at the bundle root, so `RuntimeManifest::discover()`
        // finds `run.toml` beside the (Swift-shell) executable the Xcode build /
        // sim assembler links against the runtime dylib.
        stage_payload(report, &app, &self.runtime, &self.target)?;
        // The runtime dylib + its native USD deps go into Frameworks/ (the Swift
        // shell resolves `@rpath/libidtx_runtime.dylib` there).
        stage_runtime(&self.runtime, &frameworks, &frameworks)?;

        // OpenUSD plugInfo discovery trees → Frameworks/usd (USD discovers plugins
        // relative to the directory holding libusd_ms.dylib, which stage_runtime
        // places in Frameworks/). Without them the monolithic lib cannot find
        // ArDefaultResolver on-device and USD aborts at first stage open.
        if let Some(usd_plugins) = &self.runtime.usd_plugins_dir
            && usd_plugins.is_dir() {
                copy_tree(usd_plugins, &frameworks.join("usd"))?;
            }

        // Cross-built coded OpenExec plugin. CRITICAL (visionOS linking): the
        // runtime cdylib's build script (`apps/idtx-runtime/build.rs`) forces a
        // hard `LC_LOAD_DYLIB @rpath/libidtx_usd_exec.dylib` on the runtime for
        // the visionOS target, which dyld resolves **at launch** — before any
        // `install_at` discovery runs. The bundle's only `@rpath` dir is
        // `Frameworks/` (`@executable_path/Frameworks` on the exe, `@loader_path`
        // on the runtime dylib), so the native `.dylib` must land there or the app
        // fails to load with "Library not loaded: @rpath/libidtx_usd_exec.dylib".
        // Only the plugInfo (LibraryPath emptied — the code is already resident) +
        // generatedSchema go to `<app>/usd-exec/` for `PlugRegistry` discovery.
        // Mirrors `stage_exec_plugin_android`; the old `copy_tree` into `usd-exec/`
        // (the pre-forced-load dlopen model) left the dylib unreachable at launch.
        if let Some(exec_dir) = &self.runtime.exec_plugin_dir
            && exec_dir.is_dir() {
                stage_exec_plugin_visionos(exec_dir, &frameworks, &app)?;
            }

        // Fail closed: if the runtime forces the OpenExec load but the plugin dylib
        // is not actually staged in Frameworks/, dyld would crash the app at launch.
        // Catch it here instead of shipping to device. The bundle dir is reused
        // across cooks, so a copied destination can be stale — require the *current*
        // source plugin to exist (exec dir present and holding the dylib) as well as
        // the copied destination, so an old dylib left over from a previous cook
        // cannot mask a runtime whose exec dir is now absent / empty / missing it.
        if runtime_forces_exec_load(&self.runtime.executable) {
            let source_present = self
                .runtime
                .exec_plugin_dir
                .as_ref()
                .is_some_and(|dir| dir.join("libidtx_usd_exec.dylib").is_file());
            if !source_present || !frameworks.join("libidtx_usd_exec.dylib").is_file() {
                return Err(CookError::MissingExecPlugin {
                    runtime: self.runtime.executable.clone(),
                    plugin: "libidtx_usd_exec.dylib".to_owned(),
                });
            }
        }

        // Info.plist with the visionOS immersive-space scene keys at the bundle
        // root. The `CFBundleExecutable` names the Swift shell binary the Xcode
        // build / sim assembler produces (same stem as the bundle).
        let plist = app.join("Info.plist");
        let simulator = self.target.triple.contains("-sim");
        std::fs::write(
            &plist,
            visionos_info_plist_with(&VisionOsPlistOptions {
                name: &name,
                executable: &name,
                simulator,
                bundle_id: self.bundle_id.as_deref(),
                hand_tracking: self.hand_tracking,
            }),
        )
        .map_err(|source| CookError::Io {
            path: plist.clone(),
            source,
        })?;

        crate::publication::stamp(&app, &app.join(PAYLOAD_MANIFEST))?;
        publication.publish()
    }
}

/// Options for [`visionos_info_plist_with`].
#[derive(Debug, Clone, Copy)]
#[allow(clippy::doc_markdown)]
pub struct VisionOsPlistOptions<'a> {
    /// Bundle name (`CFBundleName` / `CFBundleDisplayName`).
    pub name: &'a str,
    /// The Swift-shell binary stem (`CFBundleExecutable`).
    pub executable: &'a str,
    /// Select `XRSimulator` vs `XROS` platform strings.
    pub simulator: bool,
    /// Explicit `CFBundleIdentifier`; `None` derives `com.sap.apeirora.idtx.<name>`. For
    /// a device build this MUST match the provisioning profile's
    /// `application-identifier` or the install is rejected.
    pub bundle_id: Option<&'a str>,
    /// Add `NSHandsTrackingUsageDescription` for ARKit hand tracking (a usage
    /// string, not an entitlement — works on a free Personal Team).
    pub hand_tracking: bool,
}

/// Render the visionOS `Info.plist` for an immersive `.app`. The 3-arg
/// [`visionos_info_plist`] is a thin shim over this for existing callers.
#[must_use]
#[allow(clippy::doc_markdown)]
pub fn visionos_info_plist_with(opts: &VisionOsPlistOptions<'_>) -> String {
    let (dt_platform, supported_platform) = if opts.simulator {
        ("xrsimulator", "XRSimulator")
    } else {
        ("xros", "XROS")
    };
    let identifier = opts.bundle_id.map_or_else(
        || {
            format!(
                "com.sap.apeirora.idtx.{}",
                opts.name.to_lowercase().replace(' ', "")
            )
        },
        str::to_owned,
    );
    let name = opts.name;
    let executable = opts.executable;
    let hands = if opts.hand_tracking {
        "\n\t<key>NSHandsTrackingUsageDescription</key><string>Tracks your hands to interact with the immersive scene.</string>"
    } else {
        ""
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
\t<key>CFBundleName</key><string>{name}</string>\n\
\t<key>CFBundleDisplayName</key><string>{name}</string>\n\
\t<key>CFBundleIdentifier</key><string>{identifier}</string>\n\
\t<key>CFBundleExecutable</key><string>{executable}</string>\n\
\t<key>CFBundleVersion</key><string>1</string>\n\
\t<key>CFBundleShortVersionString</key><string>1.0</string>\n\
\t<key>CFBundlePackageType</key><string>APPL</string>\n\
\t<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>\n\
\t<key>MinimumOSVersion</key><string>2.0</string>\n\
\t<key>UIDeviceFamily</key><array><integer>7</integer></array>\n\
\t<key>DTPlatformName</key><string>{dt_platform}</string>\n\
\t<key>CFBundleSupportedPlatforms</key><array><string>{supported_platform}</string></array>\n\
\t<key>NSWorldSensingUsageDescription</key><string>Tracks your head pose to render the immersive scene stereoscopically.</string>{hands}\n\
\t<key>UIApplicationSceneManifest</key>\n\
\t<dict>\n\
\t\t<key>UIApplicationSupportsMultipleScenes</key><true/>\n\
\t\t<key>UIApplicationPreferredDefaultSceneSessionRole</key><string>UIWindowSceneSessionRoleApplication</string>\n\
\t</dict>\n\
</dict>\n\
</plist>\n",
    )
}

/// Render the visionOS `Info.plist` for an immersive `.app`. `executable` is the
/// Swift-shell binary stem (CFBundleExecutable), `simulator` selects the
/// `XRSimulator` vs `XROS` platform strings. Carries the immersive-space scene
/// manifest + the ARKit world-sensing usage description the Vision Pro pawn
/// needs for head tracking. The visionOS twin of [`MacAppInfo::to_plist`].
#[must_use]
#[allow(clippy::doc_markdown)]
pub fn visionos_info_plist(name: &str, executable: &str, simulator: bool) -> String {
    visionos_info_plist_with(&VisionOsPlistOptions {
        name,
        executable,
        simulator,
        bundle_id: None,
        hand_tracking: false,
    })
}

/// The manifest file name inside a bundle, re-exported for callers verifying a
/// produced bundle.
pub const BUNDLE_MANIFEST: &str = PAYLOAD_MANIFEST;

/// Which [`Packager`] shape a target triple wants.
///
/// Packager selection is a single decision (`.app` on Apple hosts, a portable
/// folder everywhere else), factored out so a UI or `xtask` can pick the right
/// packager from a [`CookTarget`] without duplicating the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleKind {
    /// A portable flat folder — Windows and Linux desktop/PCVR targets.
    DesktopFolder,
    /// A macOS `.app` bundle.
    MacApp,
    /// An Android `.apk` (Quest standalone) — staged under a gradle project
    /// layout, then assembled by gradle.
    AndroidApk,
    /// A visionOS `.app` bundle (Apple Vision Pro standalone) — staged under a
    /// `.app`-like layout, then assembled by Xcode.
    VisionOsApp,
}

/// The [`BundleKind`] appropriate for a Rust target `triple`.
///
/// Standalone triples are matched **first**: any `*-android` triple maps to
/// [`BundleKind::AndroidApk`], and `*-visionos`/`*-xros` triples to
/// [`BundleKind::VisionOsApp`]. Only then do the generic Apple triples
/// (`*-apple-darwin`, `*-apple-*`) map to [`BundleKind::MacApp`] (so desktop
/// macOS still works); every other host (Windows, Linux) maps to
/// [`BundleKind::DesktopFolder`].
#[must_use]
pub fn bundle_kind_for(triple: &str) -> BundleKind {
    if triple.contains("android") {
        BundleKind::AndroidApk
    } else if triple.contains("visionos") || triple.contains("xros") {
        BundleKind::VisionOsApp
    } else if triple.contains("apple") || triple.contains("darwin") {
        BundleKind::MacApp
    } else {
        BundleKind::DesktopFolder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Unique scratch dir under the system temp dir (no `tempfile` dep), matching
    /// the `cook` test convention.
    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "idtx-pkg-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A per-level cook's payload is a NESTED tree (`levels/<id>/level.usda` +
    /// bodies, `shared/`, `transfer/`, `behaviors/`). The on-device APK extractor
    /// has no directory walk — it replays `payload.manifest` by exact path — so
    /// every nested file MUST appear there (forward-slashed, self excluded) or it
    /// never reaches the device. This locks multi-level Quest packaging: with the
    /// manifest complete, `extract_apk_payload`'s `create_dir_all(parent)` recreates
    /// the nested tree and the start level's assembly + its sibling `.usdz` bodies
    /// resolve on device.
    #[test]
    fn payload_manifest_enumerates_nested_per_level_tree() {
        let assets = scratch("manifest-nested");
        let files = [
            "run.toml",
            "behaviors/login.wasm",
            "shared/player.usdz",
            "shared/globals.usdz",
            "levels/forest/level.usda",
            "levels/forest/forest.usdz",
            "levels/cave/level.usda",
            "levels/cave/cave.usdz",
            "transfer/level.usda",
            "transfer/transfer.usdz",
        ];
        for rel in files {
            let p = assets.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, b"x").unwrap();
        }

        write_payload_manifest(&assets).expect("write manifest");
        let text = fs::read_to_string(assets.join("payload.manifest")).expect("read manifest");
        let listed: std::collections::BTreeSet<&str> =
            text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();

        for rel in files {
            assert!(listed.contains(rel), "manifest missing nested payload {rel}: {text}");
        }
        assert!(
            listed.contains("levels/forest/level.usda"),
            "the start level's nested assembly must be enumerated"
        );
        assert!(
            !listed.contains("payload.manifest"),
            "the manifest must not list itself"
        );
        assert!(
            !text.contains('\\'),
            "on-device payload paths must be forward-slashed (the extractor keys on them): {text}"
        );

        let _ = fs::remove_dir_all(&assets);
    }

    /// Write a minimal but well-formed aarch64 ELF **header** stub to `path`
    /// (`\x7fELF`, 64-bit, little-endian, `e_machine = EM_AARCH64`). Enough bytes
    /// for [`verify_elf_machine`] to accept it as a real arm64 shared object; the
    /// rest of the file is irrelevant to the packager (which treats it as opaque
    /// bytes to stage).
    fn aarch64_so_stub(path: &Path) {
        let mut header = [0u8; 20];
        header[0..4].copy_from_slice(b"\x7fELF");
        header[4] = 2; // ELFCLASS64
        header[5] = 1; // ELFDATA2LSB (little-endian)
        header[16] = 3; // e_type = ET_DYN (shared object)
        header[18] = 0xB7; // e_machine = EM_AARCH64 (little-endian low byte)
        header[19] = 0x00;
        fs::write(path, header).unwrap();
        append_build_metadata(path, "aarch64-linux-android");
    }

    /// Write a minimal **Mach-O arm64** header stub to `path` (`MH_MAGIC_64`
    /// `0xFEEDFACF` + `CPU_TYPE_ARM64` `0x0100000C`). Enough bytes for
    /// [`verify_macho_arm64`] to accept it as a real arm64 dynamic library; the
    /// rest of the file is irrelevant to the packager.
    fn macho_arm64_stub(path: &Path) {
        let mut header = [0u8; 8];
        header[0..4].copy_from_slice(&[0xCF, 0xFA, 0xED, 0xFE]); // MH_MAGIC_64
        header[4..8].copy_from_slice(&0x0100_000Cu32.to_le_bytes()); // CPU_TYPE_ARM64
        fs::write(path, header).unwrap();
        append_build_metadata(path, "aarch64-apple-visionos");
    }

    fn append_build_metadata(path: &Path, target: &str) {
        use std::io::Write as _;
        let record = idtx_build_metadata::RuntimeBuildMetadata {
            schema: 1, target: target.into(), profile: "test".into(), rustc: "test compiler".into(),
            package_version: "0.1.0".into(), abi_version: idtx_behavior::ABI_VERSION,
            dependencies: idtx_build_metadata::DependencyVersions {
                glam: crate::PIN_GLAM.into(), wgpu: crate::PIN_WGPU.into(), wasmtime: crate::PIN_WASMTIME.into(),
            },
            features: vec!["authoring".into(), "physics".into(), "pcvr".into(), "quest".into(), "visionos-immersive".into(), "openexec".into(), "remote".into()],
        };
        fs::OpenOptions::new().append(true).open(path).unwrap().write_all(&record.encode().unwrap()).unwrap();
    }

    /// Build a minimal cooked payload dir (stage + run.toml + one behavior).
    fn fake_payload(dir: &Path) -> CookReport {
        fs::create_dir_all(dir.join("behaviors")).unwrap();
        fs::write(dir.join("stage.usdz"), b"USDZ").unwrap();
        fs::write(dir.join("run.toml"), b"version = 2\n").unwrap();
        fs::write(dir.join("behaviors").join("spin.wasm"), b"\0asm").unwrap();
        CookReport {
            out_dir: dir.to_path_buf(),
            stage: dir.join("stage.usdz"),
            manifest: dir.join("run.toml"),
            behaviors: Vec::new(),
            controllers: Vec::new(),
            unresolved: Vec::new(),
            external: Vec::new(),
            content_hash: "deadbeef".to_string(),
            abi_version: 2,
        }
    }

    #[test]
    fn packaging_binds_actual_runtime_pins_and_rejects_incompatible_builds() {
        let root = scratch("runtime-pins");
        let report = fake_payload(&root.join("payload"));
        let mut manifest = idtx_runtime_manifest::RuntimeManifest::load(&report.manifest).unwrap();
        manifest.pins.abi_version = Some(idtx_behavior::ABI_VERSION);
        manifest.pins.glam = Some(crate::PIN_GLAM.into());
        manifest.pins.required_features = vec!["remote".into()];
        fs::write(&report.manifest, manifest.to_toml_string()).unwrap();
        let binary = root.join("runtime.exe");
        fs::write(&binary, b"MZ").unwrap();
        append_build_metadata(&binary, "x86_64-pc-windows-msvc");
        let actual = idtx_build_metadata::RuntimeBuildMetadata::read_binary(&binary).unwrap();
        let target = CookTarget::desktop("x86_64-pc-windows-msvc");
        let package = DesktopFolderPackager::new(target, RuntimeBinary::new(&binary), root.join("dist")).with_bundle_name("Game");
        let output = package.package(&report).unwrap();
        let bound = idtx_runtime_manifest::RuntimeManifest::load(output.join("run.toml")).unwrap();
        assert_eq!(bound.pins.features, actual.features);
        assert_eq!(bound.pins.glam.as_deref(), Some(actual.dependencies.glam.as_str()));
        assert_eq!(bound.pins.abi_version, Some(actual.abi_version));
        assert_eq!(bound.pins.runtime_target.as_deref(), Some(actual.target.as_str()));
        assert_ne!(bound.stamp.content_hash.as_deref(), Some(report.content_hash.as_str()));
        assert!(idtx_runtime_manifest::RuntimeManifest::load(&report.manifest).unwrap().pins.features.is_empty());
        for case in 0..4 {
            let mut incompatible = actual.clone();
            match case {
                0 => incompatible.target = "aarch64-linux-android".into(),
                1 => incompatible.abi_version += 1,
                2 => incompatible.dependencies.glam = "0.0.0".into(),
                _ => incompatible.features.retain(|f| f != "remote"),
            }
            let mut bytes = b"MZ".to_vec();
            bytes.extend(incompatible.encode().unwrap());
            fs::write(&binary, bytes).unwrap();
            assert!(matches!(package.package(&report), Err(CookError::RuntimeMetadata(_))));
        }
        fs::write(&binary, b"MZ old unlabelled runtime").unwrap();
        assert!(matches!(package.package(&report), Err(CookError::RuntimeMetadata(_))));
    }

    #[test]
    fn android_apk_bundles_usd_plugins_and_writes_forward_slash_manifest() {
        let root = scratch("apk-usd-manifest");
        let payload = root.join("payload");
        let report = fake_payload(&payload);

        // A stand-in aarch64 runtime .so + a USD plugInfo tree to bundle.
        let so = root.join("libidtx_runtime.so");
        aarch64_so_stub(&so);
        let usd = root.join("usd");
        fs::create_dir_all(usd.join("lib").join("usd")).unwrap();
        fs::write(usd.join("lib").join("usd").join("plugInfo.json"), b"{}").unwrap();
        // A stand-in cross-built OpenExec plugin dir to bundle: a plugInfo with a
        // real LibraryPath (to exercise the emptying) + the native `.so`.
        let exec = root.join("usd-exec");
        fs::create_dir_all(&exec).unwrap();
        fs::write(
            exec.join("plugInfo.json"),
            br#"{ "Plugins": [ { "Type": "library", "LibraryPath": "libidtx_usd_exec.so", "Name": "IDTX" } ] }"#,
        )
        .unwrap();
        fs::write(exec.join("generatedSchema.usda"), b"#usda 1.0\n").unwrap();
        fs::write(exec.join("libidtx_usd_exec.so"), b"\x7fELF").unwrap();

        let runtime = RuntimeBinary {
            executable: so,
            bundled_name: Some("libidtx_runtime.so".to_owned()),
            libs_dir: None,
            usd_plugins_dir: Some(usd),
            exec_plugin_dir: Some(exec),
            resolver_plugin_dir: None,
        };
        let staging = AndroidApkPackager::new(
            CookTarget::quest("aarch64-linux-android"),
            runtime,
            root.join("out"),
        )
        .with_bundle_name("Demo")
        .package(&report)
        .unwrap();

        let assets = staging.join("assets");
        // The USD plugInfo tree is staged under assets/usd/ (so the on-device
        // runtime can point PXR_PLUGINPATH_NAME at it).
        assert!(
            assets
                .join("usd")
                .join("lib")
                .join("usd")
                .join("plugInfo.json")
                .is_file()
        );
        // The coded OpenExec plugin's NATIVE .so must land in the APK native-lib
        // dir (jniLibs/arm64-v8a/), beside libusd_ms.so — NOT in assets/ — so it
        // loads in the app linker namespace and its C++ type symbols unify with
        // the core. Its resources (plugInfo + generatedSchema) go to assets/usd-exec/.
        let jni = staging.join("jniLibs").join(ANDROID_ABI_DIR);
        assert!(
            jni.join("libidtx_usd_exec.so").is_file(),
            "OpenExec plugin .so staged into jniLibs/arm64-v8a/ (app namespace)"
        );
        assert!(
            !assets.join("usd-exec").join("libidtx_usd_exec.so").exists(),
            "the .so must NOT be in the data-dir payload (would load outside the app namespace)"
        );
        assert!(
            assets.join("usd-exec").join("plugInfo.json").is_file(),
            "OpenExec plugInfo staged under assets/usd-exec/ for PlugRegistry discovery"
        );
        assert!(assets.join("usd-exec").join("generatedSchema.usda").is_file());
        // The staged plugInfo's LibraryPath is emptied (code is resident via the
        // native-lib-dir soname load, not re-dlopened by PlugRegistry).
        let staged_pi =
            fs::read_to_string(assets.join("usd-exec").join("plugInfo.json")).unwrap();
        assert!(
            staged_pi.contains("\"LibraryPath\": \"\"") || staged_pi.contains("\"LibraryPath\":\"\""),
            "LibraryPath must be emptied: {staged_pi}"
        );

        // The manifest lists payload files with FORWARD slashes (the on-device
        // extractor opens by this path; aapt2's backslash entries are handled on
        // read), includes the staged usd tree + behaviors, and excludes itself.
        let manifest = fs::read_to_string(assets.join("payload.manifest")).unwrap();
        let lines: Vec<&str> = manifest.lines().collect();
        assert!(
            lines.contains(&"usd/lib/usd/plugInfo.json"),
            "manifest = {manifest}"
        );
        assert!(lines.contains(&"behaviors/spin.wasm"));
        assert!(lines.contains(&"run.toml"));
        assert!(
            lines.contains(&"usd-exec/plugInfo.json"),
            "exec plugInfo listed in manifest: {manifest}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("usd-exec/libidtx_usd_exec.so")),
            "the exec .so must not be in the payload manifest (it lives in jniLibs)"
        );
        assert!(
            !manifest.contains('\\'),
            "manifest must use forward slashes: {manifest}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn repackaging_removes_obsolete_files_and_hashes_native_resources() {
        let root = scratch("repackage");
        let report = fake_payload(&root.join("payload"));
        let bin = root.join("runtime.exe");
        fs::write(&bin, b"MZ").unwrap();
        append_build_metadata(&bin, "x86_64-pc-windows-msvc");
        let libs = root.join("libs");
        fs::create_dir_all(&libs).unwrap();
        fs::write(libs.join("native.dll"), b"first").unwrap();
        let pkg = DesktopFolderPackager::new(CookTarget::desktop("x86_64-pc-windows-msvc"),
            RuntimeBinary { libs_dir: Some(libs.clone()), ..RuntimeBinary::new(&bin) }, root.join("dist"));
        let output = pkg.package(&report).unwrap();
        let stamp = || fs::read(output.join(PAYLOAD_MANIFEST)).unwrap();
        let first = stamp();
        pkg.package(&report).unwrap();
        assert_eq!(first, stamp());
        fs::write(libs.join("native.dll"), b"second").unwrap();
        pkg.package(&report).unwrap();
        assert_ne!(first, stamp());
        fs::remove_file(libs.join("native.dll")).unwrap();
        fs::remove_file(report.out_dir.join("behaviors/spin.wasm")).unwrap();
        pkg.package(&report).unwrap();
        assert!(!output.join("native.dll").exists());
        assert!(!output.join("behaviors/spin.wasm").exists());
        let good = stamp();
        let result = pkg.package_with(&report, |staged| {
            fs::write(staged.join("late-plugin"), b"incomplete").unwrap();
            Err(CookError::RuntimeMetadata("late resource failed".into()))
        });
        assert!(result.is_err());
        assert_eq!(good, stamp());
        assert!(!output.join("late-plugin").exists());
        pkg.package_with(&report, |staged| {
            fs::write(staged.join("late-plugin"), b"complete").unwrap();
            Ok(())
        }).unwrap();
        assert_ne!(good, stamp());
        let good = stamp();
        fs::write(&bin, b"no metadata").unwrap();
        assert!(pkg.package(&report).is_err());
        assert_eq!(good, stamp());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn desktop_folder_bundle_has_binary_manifest_and_payload() {
        let root = scratch("desktop");
        let payload = root.join("payload");
        let report = fake_payload(&payload);

        // A stand-in runtime binary + one staged lib.
        let bin = root.join("idtx-runtime.exe");
        fs::write(&bin, b"MZ").unwrap();
        append_build_metadata(&bin, "x86_64-pc-windows-msvc");
        let libs = root.join("libs");
        fs::create_dir_all(&libs).unwrap();
        fs::write(libs.join("usd_ms.dll"), b"DLL").unwrap();

        let runtime = RuntimeBinary {
            executable: bin,
            bundled_name: None,
            libs_dir: Some(libs),
            usd_plugins_dir: None,
            exec_plugin_dir: None,
            resolver_plugin_dir: None,
        };
        let pkg = DesktopFolderPackager::new(
            CookTarget::desktop("x86_64-pc-windows-msvc"),
            runtime,
            root.join("dist"),
        )
        .with_bundle_name("MyExperience");

        let bundle = pkg.package(&report).unwrap();
        assert!(bundle.ends_with("MyExperience"));
        assert!(bundle.join("idtx-runtime.exe").is_file());
        assert!(bundle.join(BUNDLE_MANIFEST).is_file());
        assert!(bundle.join("stage.usdz").is_file());
        assert!(bundle.join("behaviors").join("spin.wasm").is_file());
        assert!(
            bundle.join("usd_ms.dll").is_file(),
            "runtime lib staged flat"
        );
    }

    #[test]
    fn mac_app_layout_places_payload_beside_binary() {
        let root = scratch("macapp");
        let payload = root.join("payload");
        let report = fake_payload(&payload);

        let bin = root.join("idtx-runtime");
        fs::write(&bin, b"\x7fELF").unwrap();
        append_build_metadata(&bin, "aarch64-apple-darwin");
        let libs = root.join("libs");
        fs::create_dir_all(&libs).unwrap();
        fs::write(libs.join("libusd_ms.dylib"), b"dylib").unwrap();

        let pkg = MacAppPackager::new(
            CookTarget::pcvr("aarch64-apple-darwin"),
            RuntimeBinary {
                executable: bin,
                bundled_name: None,
                libs_dir: Some(libs),
                usd_plugins_dir: None,
                exec_plugin_dir: None,
                resolver_plugin_dir: None,
            },
            root.join("dist"),
            MacAppInfo {
                name: "MyExperience".to_string(),
                identifier: "com.example.myexp".to_string(),
                executable: "MyExperience".to_string(),
            },
        );

        let app = pkg.package(&report).unwrap();
        assert!(app.ends_with("MyExperience.app"));
        let macos = app.join("Contents").join("MacOS");
        // Binary renamed to CFBundleExecutable, payload beside it.
        assert!(macos.join("MyExperience").is_file());
        assert!(macos.join(BUNDLE_MANIFEST).is_file());
        assert!(macos.join("stage.usdz").is_file());
        assert!(macos.join("behaviors").join("spin.wasm").is_file());
        // Libraries in Frameworks/, Info.plist emitted.
        assert!(
            app.join("Contents")
                .join("Frameworks")
                .join("libusd_ms.dylib")
                .is_file(),
            "runtime lib staged into Frameworks/"
        );
        let plist = fs::read_to_string(app.join("Contents").join("Info.plist")).unwrap();
        assert!(plist.contains("com.example.myexp"));
        assert!(plist.contains("<key>CFBundleExecutable</key><string>MyExperience</string>"));
    }

    #[test]
    fn missing_runtime_binary_is_an_error() {
        let root = scratch("missing");
        let report = fake_payload(&root.join("payload"));
        let pkg = DesktopFolderPackager::new(
            CookTarget::desktop("t"),
            RuntimeBinary::new(root.join("does-not-exist.exe")),
            root.join("dist"),
        )
        .with_bundle_name("X");
        assert!(pkg.package(&report).is_err());
    }

    #[test]
    fn bundle_kind_selects_by_triple() {
        assert_eq!(
            bundle_kind_for("x86_64-pc-windows-msvc"),
            BundleKind::DesktopFolder
        );
        assert_eq!(
            bundle_kind_for("x86_64-unknown-linux-gnu"),
            BundleKind::DesktopFolder
        );
        assert_eq!(bundle_kind_for("aarch64-apple-darwin"), BundleKind::MacApp);
        assert_eq!(bundle_kind_for("x86_64-apple-darwin"), BundleKind::MacApp);
        // Standalone triples are matched before the generic apple/darwin arm.
        assert_eq!(
            bundle_kind_for("aarch64-linux-android"),
            BundleKind::AndroidApk
        );
        assert_eq!(
            bundle_kind_for("aarch64-apple-visionos"),
            BundleKind::VisionOsApp
        );
        assert_eq!(
            bundle_kind_for("aarch64-apple-xros"),
            BundleKind::VisionOsApp
        );
    }

    #[test]
    fn android_apk_staging_has_jnilibs_and_assets_payload() {
        let root = scratch("apk");
        let payload = root.join("payload");
        let report = fake_payload(&payload);

        // A stand-in runtime cdylib + one native lib.
        let bin = root.join("libidtx_runtime.so");
        aarch64_so_stub(&bin);
        let libs = root.join("libs");
        fs::create_dir_all(&libs).unwrap();
        fs::write(libs.join("libusd_ms.so"), b"so").unwrap();

        let pkg = AndroidApkPackager::new(
            CookTarget::quest("aarch64-linux-android"),
            RuntimeBinary {
                executable: bin,
                bundled_name: None,
                libs_dir: Some(libs),
                usd_plugins_dir: None,
                exec_plugin_dir: None,
                resolver_plugin_dir: None,
            },
            root.join("dist"),
        )
        .with_bundle_name("MyExperience");

        let staging = pkg.package(&report).unwrap();
        assert!(staging.ends_with("MyExperience"));
        let jni = staging.join("jniLibs").join(ANDROID_ABI_DIR);
        // Runtime cdylib + native lib both under jniLibs/arm64-v8a/.
        assert!(jni.join("libidtx_runtime.so").is_file());
        assert!(
            jni.join("libusd_ms.so").is_file(),
            "native lib staged into jniLibs/arm64-v8a/"
        );
        // Payload + run.toml bundled under assets/ (extracted on-device).
        let assets = staging.join("assets");
        assert!(assets.join(BUNDLE_MANIFEST).is_file());
        assert!(assets.join("stage.usdz").is_file());
        assert!(assets.join("behaviors").join("spin.wasm").is_file());

        // Minimal gradle project at the module root, with placeholders filled.
        let manifest = fs::read_to_string(staging.join("AndroidManifest.xml")).unwrap();
        assert!(manifest.contains("package=\"com.idtx.myexperience\""));
        assert!(manifest.contains("android:label=\"MyExperience\""));
        assert!(manifest.contains(&format!("android:value=\"{ANDROID_RUNTIME_LIB_NAME}\"")));
        assert!(!manifest.contains("{{"), "no placeholders left in manifest");
        let build_gradle = fs::read_to_string(staging.join("build.gradle")).unwrap();
        assert!(build_gradle.contains("applicationId 'com.idtx.myexperience'"));
        assert!(build_gradle.contains("arm64-v8a"));
        let settings = fs::read_to_string(staging.join("settings.gradle")).unwrap();
        assert!(settings.contains("rootProject.name = 'MyExperience'"));
    }

    /// Reject the XML-comment well-formedness rule aapt2 enforces: a comment
    /// body (between `<!--` and the terminating `-->`) may not contain `--`.
    /// Returns the offending body on violation. A hand-rolled check (rather than
    /// pulling in an XML parser) keeps the dep surface flat while catching the
    /// exact "not well-formed (invalid token)" class that aapt2 rejects.
    fn xml_comment_bodies_double_hyphen_free(xml: &str) -> Result<(), String> {
        let mut rest = xml;
        while let Some(start) = rest.find("<!--") {
            let after = &rest[start + 4..];
            let end = after
                .find("-->")
                .ok_or_else(|| "unterminated XML comment".to_owned())?;
            let body = &after[..end];
            if body.contains("--") {
                return Err(body.to_owned());
            }
            rest = &after[end + 3..];
        }
        Ok(())
    }

    #[test]
    fn android_manifest_template_is_comment_well_formed() {
        // Guards the shipped template directly (the gated end-to-end aapt2 test
        // uses an inline manifest, so a `--`-in-comment regression there slips
        // through). Substitute representative placeholders first so the exact
        // bytes aapt2 would link are what we validate.
        let manifest = ANDROID_MANIFEST_TEMPLATE
            .replace("{{PACKAGE}}", "com.idtx.demo")
            .replace("{{LABEL}}", "Demo")
            .replace("{{LIB_NAME}}", ANDROID_RUNTIME_LIB_NAME);
        if let Err(body) = xml_comment_bodies_double_hyphen_free(&manifest) {
            panic!("AndroidManifest.xml comment contains illegal `--`: {body:?}");
        }
        // The checker itself must catch the real bug shape.
        assert!(
            xml_comment_bodies_double_hyphen_free("<!-- a --min-sdk b -->").is_err(),
            "checker must flag `--` inside a comment body"
        );
        assert!(xml_comment_bodies_double_hyphen_free("<!-- fine -->").is_ok());
    }

    #[test]
    fn android_manifest_has_no_required_uses_native_library() {
        // Quest does not expose libopenxr_loader.so as a reconcilable system
        // library — the loader is bundled in lib/arm64-v8a/. A required
        // uses-native-library for it is the direct cause of
        // INSTALL_FAILED_MISSING_SHARED_LIBRARY, so the template must not carry
        // one.
        assert!(
            !ANDROID_MANIFEST_TEMPLATE.contains("<uses-native-library"),
            "manifest template must not declare a system <uses-native-library> element"
        );
    }

    #[test]
    fn empty_library_path_neuters_value_and_preserves_rest() {
        let src = r#"{ "Plugins": [ { "Type": "library", "LibraryPath": "libidtx_usd_exec.so", "Name": "IDTX" } ] }"#;
        let out = empty_plug_info_library_path(src);
        assert!(out.contains(r#""LibraryPath": """#), "value emptied: {out}");
        assert!(!out.contains("libidtx_usd_exec.so"), "old value gone: {out}");
        // Other keys/values are untouched.
        assert!(out.contains(r#""Type": "library""#));
        assert!(out.contains(r#""Name": "IDTX""#));
        // A plugInfo without the key is returned unchanged.
        let none = r#"{ "Includes": [ "*/resources/" ] }"#;
        assert_eq!(empty_plug_info_library_path(none), none);
    }

    #[test]
    fn android_package_id_slugifies_and_falls_back() {        assert_eq!(android_package_id("MyExperience"), "com.idtx.myexperience");
        assert_eq!(android_package_id("Demo 1!"), "com.idtx.demo1");
        assert_eq!(android_package_id("___"), "com.idtx.experience");
    }

    #[test]
    fn android_package_id_override_wins() {
        let root = scratch("apk-pkgid");
        let payload = root.join("payload");
        let report = fake_payload(&payload);
        let bin = root.join("libidtx_runtime.so");
        aarch64_so_stub(&bin);

        let pkg = AndroidApkPackager::new(
            CookTarget::quest("aarch64-linux-android"),
            RuntimeBinary::new(bin),
            root.join("dist"),
        )
        .with_bundle_name("MyExperience")
        .with_package_id("com.acme.vrapp");

        let staging = pkg.package(&report).unwrap();
        let manifest = fs::read_to_string(staging.join("AndroidManifest.xml")).unwrap();
        assert!(manifest.contains("package=\"com.acme.vrapp\""));
    }

    #[test]
    fn android_rejects_a_windows_pe_runtime() {
        // The field regression: a host Windows `idtx-runtime.exe` (PE / `MZ`
        // magic) must never be staged into jniLibs/arm64-v8a/. The Android
        // packager fails closed with a precise error naming the bad binary.
        let root = scratch("apk-pe");
        let report = fake_payload(&root.join("payload"));
        let bin = root.join("idtx-runtime.exe");
        // `MZ` + filler — a stand-in for a real PE header.
        fs::write(
            &bin,
            b"MZ\x90\x00\x03\x00\x00\x00\x04\x00\x00\x00\xff\xff\x00\x00\xb8\x00\x00\x00",
        )
        .unwrap();

        let pkg = AndroidApkPackager::new(
            CookTarget::quest("aarch64-linux-android"),
            RuntimeBinary::new(bin.clone()),
            root.join("dist"),
        )
        .with_bundle_name("MyExperience");

        match pkg.package(&report) {
            Err(CookError::InvalidRuntimeBinary { path, detail, .. }) => {
                assert_eq!(path, bin);
                assert!(detail.contains("PE"), "detail should name the PE: {detail}");
            }
            other => panic!("expected InvalidRuntimeBinary, got {other:?}"),
        }
    }

    #[test]
    fn android_rejects_a_wrong_arch_elf_runtime() {
        // A non-aarch64 ELF (e.g. an x86-64 host .so accidentally handed in) is
        // also rejected — only a real arm64 object may reach the APK.
        let root = scratch("apk-x64elf");
        let report = fake_payload(&root.join("payload"));
        let bin = root.join("libidtx_runtime.so");
        let mut header = [0u8; 20];
        header[0..4].copy_from_slice(b"\x7fELF");
        header[4] = 2; // ELFCLASS64
        header[5] = 1; // little-endian
        header[18] = 0x3e; // e_machine = EM_X86_64 (62), not aarch64
        header[19] = 0x00;
        fs::write(&bin, header).unwrap();

        let pkg = AndroidApkPackager::new(
            CookTarget::quest("aarch64-linux-android"),
            RuntimeBinary::new(bin),
            root.join("dist"),
        )
        .with_bundle_name("MyExperience");

        assert!(matches!(
            pkg.package(&report),
            Err(CookError::InvalidRuntimeBinary { .. })
        ));
    }

    #[test]
    fn visionos_app_layout_places_payload_beside_binary() {
        let root = scratch("visionos");
        let payload = root.join("payload");
        let report = fake_payload(&payload);

        let bin = root.join("idtx-runtime");
        macho_arm64_stub(&bin);
        let libs = root.join("libs");
        fs::create_dir_all(&libs).unwrap();
        fs::write(libs.join("libusd_ms.dylib"), b"dylib").unwrap();
        // A USD plugInfo tree to embed in Frameworks/usd.
        let usd = root.join("usd");
        fs::create_dir_all(usd.join("lib").join("usd")).unwrap();
        fs::write(usd.join("lib").join("usd").join("plugInfo.json"), b"{}").unwrap();
        // A stand-in cross-built OpenExec plugin dir: a native dylib + a plugInfo
        // with a real LibraryPath (to exercise the emptying) + a schema resource.
        let exec = root.join("usd-exec");
        fs::create_dir_all(&exec).unwrap();
        fs::write(
            exec.join("plugInfo.json"),
            br#"{ "Plugins": [ { "Type": "library", "LibraryPath": "libidtx_usd_exec.dylib", "Name": "IDTX" } ] }"#,
        )
        .unwrap();
        fs::write(exec.join("libidtx_usd_exec.dylib"), b"dylib").unwrap();
        fs::write(exec.join("generatedSchema.usda"), b"#usda 1.0\n").unwrap();

        let pkg = VisionOsAppPackager::new(
            CookTarget::vision_pro("aarch64-apple-visionos"),
            RuntimeBinary {
                executable: bin,
                bundled_name: None,
                libs_dir: Some(libs),
                usd_plugins_dir: Some(usd),
                exec_plugin_dir: Some(exec),
                resolver_plugin_dir: None,
            },
            root.join("dist"),
        )
        .with_bundle_name("MyExperience");

        let app = pkg.package(&report).unwrap();
        assert!(app.ends_with("MyExperience.app"));
        // visionOS `.app`s are FLAT: payload + run.toml + Info.plist at the bundle
        // root (where RuntimeManifest::discover finds run.toml beside the exe),
        // Frameworks/ at the root — NOT a macOS-style Contents/.
        assert!(
            !app.join("Contents").exists(),
            "visionOS bundle must be flat"
        );
        assert!(app.join(BUNDLE_MANIFEST).is_file());
        assert!(app.join("stage.usdz").is_file());
        assert!(app.join("behaviors").join("spin.wasm").is_file());
        // Info.plist with the immersive scene keys at the bundle root.
        let plist = fs::read_to_string(app.join("Info.plist")).unwrap();
        assert!(
            plist.contains("<string>XRSimulator</string>")
                || plist.contains("<string>XROS</string>")
        );
        assert!(plist.contains("NSWorldSensingUsageDescription"));
        assert!(plist.contains("<integer>7</integer>")); // visionOS device family
        // Runtime dylib + USD dylibs embedded in Frameworks/.
        let frameworks = app.join("Frameworks");
        assert!(
            frameworks.join("idtx-runtime").is_file(),
            "runtime binary staged into Frameworks/"
        );
        assert!(
            frameworks.join("libusd_ms.dylib").is_file(),
            "USD lib staged into Frameworks/"
        );
        // USD plugInfo trees beside the dylibs (where USD discovers them).
        assert!(
            frameworks
                .join("usd")
                .join("lib")
                .join("usd")
                .join("plugInfo.json")
                .is_file(),
            "USD plugInfo tree staged into Frameworks/usd/"
        );
        // The coded OpenExec plugin's native dylib goes into Frameworks/ (the
        // bundle's only @rpath dir) so dyld resolves the runtime's forced
        // `LC_LOAD_DYLIB @rpath/libidtx_usd_exec.dylib` at launch. It must NOT be
        // left in usd-exec/ (not an rpath) or the app fails to load.
        assert!(
            frameworks.join("libidtx_usd_exec.dylib").is_file(),
            "OpenExec dylib staged into Frameworks/ (resolves the forced @rpath load)"
        );
        assert!(
            !app.join("usd-exec").join("libidtx_usd_exec.dylib").exists(),
            "OpenExec dylib must not stay in usd-exec/ (not on the bundle rpath)"
        );
        // Only discovery resources sit at <app>/usd-exec/, where the immersive
        // entrypoint's `install_at` registers the schema/compute metadata.
        assert!(
            app.join("usd-exec").join("plugInfo.json").is_file(),
            "OpenExec plugInfo staged at <app>/usd-exec/ for PlugRegistry discovery"
        );
        assert!(
            app.join("usd-exec").join("generatedSchema.usda").is_file(),
            "OpenExec generatedSchema staged at <app>/usd-exec/"
        );
        // LibraryPath emptied so PlugRegistry registers metadata without
        // re-dlopening the code (already resident via the Frameworks/ forced load).
        let staged_pi =
            fs::read_to_string(app.join("usd-exec").join("plugInfo.json")).unwrap();
        assert!(
            staged_pi.contains("\"LibraryPath\": \"\"") || staged_pi.contains("\"LibraryPath\":\"\""),
            "LibraryPath must be emptied: {staged_pi}"
        );
    }

    #[test]
    fn runtime_forces_exec_load_detects_dependency() {
        let root = scratch("forces-exec");
        // A runtime whose bytes carry the plugin load-command name is detected…
        let with_dylib = root.join("with_dylib.bin");
        fs::write(&with_dylib, b"\xCF\xFA\xED\xFE....@rpath/libidtx_usd_exec.dylib\0....").unwrap();
        assert!(runtime_forces_exec_load(&with_dylib));
        let with_so = root.join("with_so.bin");
        fs::write(&with_so, b"\x7fELF....libidtx_usd_exec.so\0....").unwrap();
        assert!(runtime_forces_exec_load(&with_so));
        // …and a runtime without it (openexec feature off) is not.
        let without = root.join("without.bin");
        fs::write(&without, b"\xCF\xFA\xED\xFE no exec dependency here").unwrap();
        assert!(!runtime_forces_exec_load(&without));
        // A missing file is treated as "not forced" (the format guards elsewhere
        // already reject unreadable runtimes).
        assert!(!runtime_forces_exec_load(&root.join("absent.bin")));
    }

    #[test]
    fn visionos_app_fails_closed_when_exec_forced_but_unstaged() {
        // A runtime built with `openexec` carries a forced
        // `@rpath/libidtx_usd_exec.dylib` load that dyld resolves at launch. If the
        // plugin is not staged into Frameworks/ (here: no exec_plugin_dir at all),
        // the packager must FAIL rather than ship a `.app` that dyld-crashes before
        // `main`. Regression guard for the Vision Pro "Library not loaded" crash.
        let root = scratch("visionos-exec-missing");
        let report = fake_payload(&root.join("payload"));

        // A valid Mach-O arm64 stub whose bytes embed the forced-load name.
        let bin = root.join("libidtx_runtime.dylib");
        let mut bytes = vec![0xCF, 0xFA, 0xED, 0xFE];
        bytes.extend_from_slice(&0x0100_000Cu32.to_le_bytes()); // CPU_TYPE_ARM64
        bytes.extend_from_slice(b"\x00\x00@rpath/libidtx_usd_exec.dylib\x00");
        fs::write(&bin, &bytes).unwrap();
        append_build_metadata(&bin, "aarch64-apple-visionos");
        let libs = root.join("libs");
        fs::create_dir_all(&libs).unwrap();
        fs::write(libs.join("libusd_ms.dylib"), b"dylib").unwrap();

        let err = VisionOsAppPackager::new(
            CookTarget::vision_pro("aarch64-apple-visionos"),
            RuntimeBinary {
                executable: bin.clone(),
                bundled_name: None,
                libs_dir: Some(libs),
                usd_plugins_dir: None,
                exec_plugin_dir: None, // exec forced by the runtime, but nothing staged
                resolver_plugin_dir: None,
            },
            root.join("dist"),
        )
        .with_bundle_name("MyExperience")
        .package(&report)
        .unwrap_err();
        assert!(
            matches!(err, CookError::MissingExecPlugin { ref plugin, .. } if plugin == "libidtx_usd_exec.dylib"),
            "expected MissingExecPlugin, got {err:?}"
        );
    }

    #[test]
    fn android_apk_fails_closed_when_exec_forced_but_unstaged() {
        // Android twin of the visionOS guard: a runtime that forces
        // `DT_NEEDED libidtx_usd_exec.so` but no staged plugin must fail the pack.
        let root = scratch("android-exec-missing");
        let report = fake_payload(&root.join("payload"));

        // A valid aarch64 ELF stub whose bytes embed the DT_NEEDED name.
        let so = root.join("libidtx_runtime.so");
        let mut bytes = vec![0x7f, b'E', b'L', b'F', 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        bytes.push(3); // e_type = ET_DYN
        bytes.extend_from_slice(&[0, 0xB7, 0x00]);
        bytes.extend_from_slice(b"\x00libidtx_usd_exec.so\x00");
        fs::write(&so, &bytes).unwrap();
        append_build_metadata(&so, "aarch64-linux-android");

        let err = AndroidApkPackager::new(
            CookTarget::quest("aarch64-linux-android"),
            RuntimeBinary {
                executable: so,
                bundled_name: Some("libidtx_runtime.so".to_owned()),
                libs_dir: None,
                usd_plugins_dir: None,
                exec_plugin_dir: None,
                resolver_plugin_dir: None,
            },
            root.join("out"),
        )
        .with_bundle_name("Demo")
        .package(&report)
        .unwrap_err();
        assert!(
            matches!(err, CookError::MissingExecPlugin { ref plugin, .. } if plugin == "libidtx_usd_exec.so"),
            "expected MissingExecPlugin, got {err:?}"
        );
    }

    #[test]
    fn android_apk_bundles_http_resolver_plugin() {
        // The cross-built HTTP resolver plugin's NATIVE .so must land in the APK
        // native-lib dir (jniLibs/arm64-v8a/) — it is a forced DT_NEEDED of the
        // runtime — while only its (LibraryPath-emptied) plugInfo rides along in
        // assets/idtx-http-resolver/. Twin of the OpenExec staging test.
        let root = scratch("apk-http-resolver");
        let payload = root.join("payload");
        let report = fake_payload(&payload);

        let so = root.join("libidtx_runtime.so");
        aarch64_so_stub(&so);
        // A stand-in cross-built resolver dir: a plugInfo with a real LibraryPath
        // (to exercise the emptying) + the native `.so`, both directly in the dir
        // (matching the resolver build script's flat `out/` layout).
        let resolver = root.join("idtx-http-resolver");
        fs::create_dir_all(&resolver).unwrap();
        fs::write(
            resolver.join("plugInfo.json"),
            br#"{ "Plugins": [ { "Type": "library", "LibraryPath": "libidtx_http_resolver.so", "Name": "IdtxHttp" } ] }"#,
        )
        .unwrap();
        fs::write(resolver.join("libidtx_http_resolver.so"), b"\x7fELF").unwrap();

        let runtime = RuntimeBinary {
            executable: so,
            bundled_name: Some("libidtx_runtime.so".to_owned()),
            libs_dir: None,
            usd_plugins_dir: None,
            exec_plugin_dir: None,
            resolver_plugin_dir: Some(resolver),
        };
        let staging = AndroidApkPackager::new(
            CookTarget::quest("aarch64-linux-android"),
            runtime,
            root.join("out"),
        )
        .with_bundle_name("Demo")
        .package(&report)
        .unwrap();

        let assets = staging.join("assets");
        let jni = staging.join("jniLibs").join(ANDROID_ABI_DIR);
        assert!(
            jni.join("libidtx_http_resolver.so").is_file(),
            "resolver .so staged into jniLibs/arm64-v8a/ (resolves the DT_NEEDED)"
        );
        assert!(
            !assets
                .join(idtx_standalone::HTTP_RESOLVER_PLUGIN_DIR_NAME)
                .join("libidtx_http_resolver.so")
                .exists(),
            "the .so must NOT be in the data-dir payload"
        );
        let staged_pi = fs::read_to_string(
            assets
                .join(idtx_standalone::HTTP_RESOLVER_PLUGIN_DIR_NAME)
                .join("plugInfo.json"),
        )
        .unwrap();
        assert!(
            staged_pi.contains("\"LibraryPath\": \"\"") || staged_pi.contains("\"LibraryPath\":\"\""),
            "LibraryPath must be emptied: {staged_pi}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn android_apk_fails_closed_when_resolver_forced_but_unstaged() {
        // A runtime that forces `DT_NEEDED libidtx_http_resolver.so` (the `remote`
        // feature) but has no staged resolver plugin must fail the pack rather than
        // ship an APK that dies at launch with UnsatisfiedLinkError. Regression
        // guard for the observed Quest crash.
        let root = scratch("android-resolver-missing");
        let report = fake_payload(&root.join("payload"));

        // A valid aarch64 ELF stub whose bytes embed the DT_NEEDED name.
        let so = root.join("libidtx_runtime.so");
        let mut bytes = vec![0x7f, b'E', b'L', b'F', 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        bytes.push(3); // e_type = ET_DYN
        bytes.extend_from_slice(&[0, 0xB7, 0x00]);
        bytes.extend_from_slice(b"\x00libidtx_http_resolver.so\x00");
        fs::write(&so, &bytes).unwrap();
        append_build_metadata(&so, "aarch64-linux-android");

        let err = AndroidApkPackager::new(
            CookTarget::quest("aarch64-linux-android"),
            RuntimeBinary {
                executable: so,
                bundled_name: Some("libidtx_runtime.so".to_owned()),
                libs_dir: None,
                usd_plugins_dir: None,
                exec_plugin_dir: None,
                resolver_plugin_dir: None, // resolver forced by the runtime, but nothing staged
            },
            root.join("out"),
        )
        .with_bundle_name("Demo")
        .package(&report)
        .unwrap_err();
        assert!(
            matches!(err, CookError::MissingResolverPlugin { ref plugin, .. } if plugin == "libidtx_http_resolver.so"),
            "expected MissingResolverPlugin, got {err:?}"
        );
    }

    #[test]
    fn visionos_app_rejects_non_macho_runtime() {
        // The Vision Pro packager must fail closed on a host binary: a `.dylib`
        // input that is actually an aarch64 ELF (or a PE) can never be staged into
        // the `.app` Frameworks/. Mirrors the Android ELF-guard regression test.
        let root = scratch("visionos-reject");
        let report = fake_payload(&root.join("payload"));
        let bin = root.join("libidtx_runtime.dylib");
        aarch64_so_stub(&bin); // an ELF masquerading as a .dylib

        let pkg = VisionOsAppPackager::new(
            CookTarget::vision_pro("aarch64-apple-visionos"),
            RuntimeBinary::new(bin),
            root.join("dist"),
        )
        .with_bundle_name("MyExperience");

        assert!(matches!(
            pkg.package(&report),
            Err(CookError::InvalidRuntimeBinary { .. })
        ));
    }

    #[test]
    fn visionos_info_plist_device_keys_bundle_id_and_hand_tracking() {
        // Device slice: XROS platform strings, an overridable bundle id (must
        // match the signing profile on device), and the opt-in hand-tracking key.
        let device = visionos_info_plist_with(&VisionOsPlistOptions {
            name: "My Experience",
            executable: "My Experience",
            simulator: false,
            bundle_id: Some("com.example.myexp"),
            hand_tracking: true,
        });
        assert!(device.contains("<string>XROS</string>"));
        assert!(device.contains("<string>xros</string>"));
        assert!(device.contains("<key>CFBundleIdentifier</key><string>com.example.myexp</string>"));
        assert!(device.contains("NSHandsTrackingUsageDescription"));
        // Head-pose world sensing is always present.
        assert!(device.contains("NSWorldSensingUsageDescription"));

        // Defaults: derived id, no hand tracking, simulator platform.
        let sim = visionos_info_plist("Demo", "Demo", true);
        assert!(sim.contains("<string>XRSimulator</string>"));
        assert!(
            sim.contains(
                "<key>CFBundleIdentifier</key><string>com.sap.apeirora.idtx.demo</string>"
            )
        );
        assert!(!sim.contains("NSHandsTrackingUsageDescription"));
    }
}
