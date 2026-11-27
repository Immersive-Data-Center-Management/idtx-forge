//! Build the IDTX `OpenExec` plugin as an `OpenUSD` plugin DLL/dylib that
//! `PlugRegistry` dlopens, and stage its `plugInfo.json` + `generatedSchema.usda`
//! beside it.
//!
//! Unlike the *codeless* `idtx-usd-extension` (pure resource data, no C++), this
//! crate carries compiled computations: each `cpp/computation_*.cpp` registers an
//! `OpenExec` computation via `EXEC_REGISTER_COMPUTATIONS_FOR_SCHEMA`, and each
//! generated `compute_*.cpp` / `datasource.cpp` etc. registers a `TfType` schema.
//! Those are **static initializers** that run at DLL load.
//!
//! # Why cl.exe/link.exe directly (not cc::Build / crate-type cdylib)
//!
//! The registrations are `TF_REGISTRY_FUNCTION` static initializers with no
//! referenced symbol. If the objects were archived into a static `.lib` and
//! pulled into a Rust binary, the linker would drop the "unreferenced" members
//! and the registrations would silently vanish (`GetValueKeyCount()` stays 0).
//! We instead compile each `cpp/*.cpp` to its own `.obj` and pass every object
//! **directly** on the `link.exe /DLL` line — directly-named objects are never
//! discarded, so no `/WHOLEARCHIVE` is needed. This mirrors
//! `crates/idtx-usd-resolver-http/build.rs` (the in-repo precedent for a
//! cc-built USD plugin DLL), extended from one source to the whole `cpp/` dir.
//!
//! The crate itself is a normal rlib; the DLL is a **side artifact** in `OUT_DIR`
//! that the Rust side never links against — `PlugRegistry` loads it at runtime
//! from the baked [`IDTX_USD_EXEC_PLUGIN_DIR`] directory.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=IDTX_USD_DIR");
    println!("cargo:rerun-if-env-changed=IDTX_USD_DIR_ANDROID");
    println!("cargo:rerun-if-env-changed=IDTX_USD_DIR_VISIONOS");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=cpp");
    println!("cargo:rerun-if-changed=resources/plugInfo.json");
    println!("cargo:rerun-if-changed=resources/generatedSchema.usda");

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let target = env::var("TARGET").unwrap_or_default();
    // In a build script `cfg!(target_os = …)` is the HOST, not the compilation
    // target — a landmine when cross-compiling (e.g. a Windows host building for
    // `aarch64-linux-android`). The real target OS is in `CARGO_CFG_TARGET_OS`;
    // every host-vs-target decision below keys off it. Mirrors `idtx-usd-sys`.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // Target-aware USD prefix: host → `$IDTX_USD_DIR`; android/visionos → the
    // cross-build staging prefix (`$IDTX_USD_DIR_<PLAT>` → `thirdparty/openusd-<plat>`).
    let usd_dir = idtx_usd_build_support::resolve_usd_dir_for_target(&manifest, &target_os);
    let usd_include = usd_dir.join("include");
    let usd_lib = usd_dir.join("lib");
    let cpp_dir = manifest.join("cpp");

    // Standalone (android/visionos): oneTBB lives in a SEPARATE sibling prefix, not
    // inside the USD install. The exec computations pull TBB via inline
    // `pxr/exec/vdf/context.h` templates, so its `include/` must be on the compile
    // path and its `lib/` on the link search. MaterialX is not linked by this
    // plugin, so `want_materialx = false`.
    let sibling_prefixes = match target_os.as_str() {
        "android" => idtx_usd_build_support::standalone_sibling_prefixes(
            &usd_dir,
            &manifest,
            false,
            ("IDTX_TBB_DIR_ANDROID", "onetbb-android"),
            ("IDTX_MATERIALX_DIR_ANDROID", "materialx-android"),
        ),
        "visionos" => idtx_usd_build_support::standalone_sibling_prefixes(
            &usd_dir,
            &manifest,
            false,
            ("IDTX_TBB_DIR_VISIONOS", "onetbb-visionos"),
            ("IDTX_MATERIALX_DIR_VISIONOS", "materialx-visionos"),
        ),
        _ => Vec::new(),
    };

    // The plugin dir holds the DLL + plugInfo.json + generatedSchema.usda as
    // siblings — the flat layout PlugRegistry::RegisterPlugins() expects
    // (Root="." / ResourcePath="." resolve relative to plugInfo.json's own dir).
    let plugin_dir = out_dir.join("idtx-usd-exec");
    fs::create_dir_all(&plugin_dir).expect("create plugin dir in OUT_DIR");

    // MSVC is only correct for a `windows` *target* (host or a windows cross). On a
    // Windows host cross-building for Android/visionOS the plugin is built by the
    // NDK/SDK clang (via `CXX_<triple>`), so priming MSVC there would be wrong.
    if target_os == "windows" {
        idtx_usd_build_support::prime_msvc_env();
    }

    let cpp_sources = collect_cpp_sources(&cpp_dir);
    assert!(
        !cpp_sources.is_empty(),
        "no .cpp sources found in {}",
        cpp_dir.display()
    );

    let lib_path = if target_os == "windows" {
        build_plugin_dll_msvc(&cpp_sources, &cpp_dir, &usd_include, &usd_lib, &plugin_dir)
    } else {
        build_plugin_shared_clang(
            &cpp_sources,
            &cpp_dir,
            &usd_include,
            &usd_lib,
            &sibling_prefixes,
            &plugin_dir,
            &target,
            &target_os,
        )
    };

    stage_resources(&manifest, &plugin_dir, &lib_path);

    println!(
        "cargo:rustc-env=IDTX_USD_EXEC_PLUGIN_DIR={}",
        plugin_dir.display()
    );

    // Export the staged plugin dir to DIRECT dependents' build scripts (via the
    // crate's `links` key) as `DEP_IDTX_USD_EXEC_PLUGIN_DIR`. The runtime cdylib's
    // build script consumes it to force a `DT_NEEDED` on the plugin for standalone
    // targets. We deliberately do NOT emit `rustc-link-lib` here: it propagates to
    // the runtime link, but Rust's default `--as-needed` drops the unreferenced
    // dependency, and the `--no-as-needed` link-arg does not propagate from a
    // dependency — so the final-binary's own build script must drive the link.
    println!("cargo:PLUGIN_DIR={}", plugin_dir.display());
}

/// All `*.cpp` under `cpp/`, sorted for deterministic builds.
fn collect_cpp_sources(cpp_dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(cpp_dir)
        .unwrap_or_else(|e| panic!("read cpp dir {}: {e}", cpp_dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("cpp"))
        .collect();
    v.sort();
    v
}

/// Compile every `cpp/*.cpp` to its own `.obj`, then link all objects directly
/// into one DLL via `link.exe /DLL`. Objects are passed directly (not archived)
/// so the `TF_REGISTRY_FUNCTION` / `EXEC_REGISTER_COMPUTATIONS_FOR_SCHEMA` static
/// initializers survive — the MSVC whole-archive-equivalent.
fn build_plugin_dll_msvc(
    sources: &[PathBuf],
    cpp_dir: &Path,
    usd_include: &Path,
    usd_lib: &Path,
    plugin_dir: &Path,
) -> PathBuf {
    let mut objs = Vec::with_capacity(sources.len());
    for src in sources {
        let obj = plugin_dir
            .join(src.file_stem().unwrap())
            .with_extension("obj");
        let status = Command::new("cl.exe")
            .args(["/c", "/nologo", "/std:c++17", "/O2", "/MD", "/EHsc", "/GR"])
            // IDTX_EXPORTS makes api.h's IDTX_API expand to __declspec(dllexport)
            // so the generated schema classes export their vtable/type_info.
            .args(["/DNOMINMAX", "/DWIN32_LEAN_AND_MEAN", "/DIDTX_EXPORTS"])
            .args(["/wd4996", "/wd4273", "/wd4244", "/wd4267"])
            // cpp/ resolves both the "./foo.h" relative includes and the
            // <tokens.h> / <EnvironmentProvider.h> angle-bracket includes.
            .arg(format!("/I{}", cpp_dir.display()))
            .arg(format!("/I{}", usd_include.display()))
            .arg(format!("/Fo:{}", obj.display()))
            .arg(src)
            .status()
            .unwrap_or_else(|e| panic!("failed to invoke cl.exe: {e}"));
        assert!(status.success(), "cl.exe failed on {}", src.display());
        objs.push(obj);
    }

    let dll = plugin_dir.join("idtx_usd_exec.dll");
    let implib = plugin_dir.join("idtx_usd_exec.lib");
    let mut cmd = Command::new("link.exe");
    cmd.args(["/DLL", "/NOLOGO"])
        .arg(format!("/OUT:{}", dll.display()))
        .arg(format!("/IMPLIB:{}", implib.display()))
        .arg(format!("/LIBPATH:{}", usd_lib.display()))
        // Monolithic usd_ms carries the exec (Vdf/Exec/EfTime) + Usd symbols;
        // tbb is pulled by inline templates in pxr/exec/vdf/context.h.
        .arg("usd_ms.lib")
        .arg("tbb.lib");
    for o in &objs {
        cmd.arg(o);
    }
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke link.exe: {e}"));
    assert!(status.success(), "link.exe /DLL of idtx_usd_exec failed");
    dll
}

/// Compile the plugin as a shared library with a clang-like driver — the host
/// macOS/Linux path **and** the Android/visionOS cross path.
///
/// A single compiler invocation over all sources keeps the `TF_REGISTRY_FUNCTION`
/// / `EXEC_REGISTER_COMPUTATIONS_FOR_SCHEMA` static initializers in the output
/// (nothing is archived, so nothing is dropped as "unreferenced").
///
/// # Cross-compilation
///
/// The compiler and its target flags come from the same target-scoped env vars
/// the runtime cross-builds (`cargo xtask build-quest-runtime` /
/// `build-visionos-runtime`) already set for the cxx bridge:
/// * `CXX_<triple>` — the NDK clang++ (Android, already embeds `--target`+sysroot)
///   or the `xcrun clang++` (visionOS). Falls back to `CXX`, then `clang++`.
/// * `CXXFLAGS_<triple>` — the visionOS `-isysroot <sdk> -target arm64-apple-xros…`
///   (empty for Android, whose clang wrapper carries them).
///
/// So the exec plugin cross-compiles under the identical environment as
/// `idtx-usd-sys`, against the matching cross USD + sibling oneTBB prefixes.
#[allow(clippy::too_many_arguments)] // a build-script helper threading resolved paths + target info
fn build_plugin_shared_clang(
    sources: &[PathBuf],
    cpp_dir: &Path,
    usd_include: &Path,
    usd_lib: &Path,
    sibling_prefixes: &[PathBuf],
    plugin_dir: &Path,
    target: &str,
    target_os: &str,
) -> PathBuf {
    // macOS + visionOS ship a Mach-O `.dylib`; linux + android an ELF `.so`.
    let apple = target_os == "macos" || target_os == "visionos";
    let (name, shared_flag) = if apple {
        ("libidtx_usd_exec.dylib", "-dynamiclib")
    } else {
        ("libidtx_usd_exec.so", "-shared")
    };
    let output = plugin_dir.join(name);

    // Target-scoped compiler + flags (cross), falling back to the host `CXX` /
    // `clang++` for a native build.
    let compiler = env_for_target("CXX", target).unwrap_or_else(|| "clang++".to_owned());
    let extra_flags: Vec<String> = env_for_target("CXXFLAGS", target)
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();

    let mut cmd = Command::new(&compiler);
    cmd.args(&extra_flags)
        .arg(shared_flag)
        .arg("-std=c++17")
        .arg("-fPIC")
        .arg("-O2")
        .arg("-fvisibility=default")
        .arg("-Wno-deprecated-declarations")
        .arg("-DNOMINMAX")
        .arg("-DIDTX_EXPORTS")
        .arg("-o")
        .arg(&output)
        .arg(format!("-I{}", cpp_dir.display()))
        .arg(format!("-I{}", usd_include.display()));
    // Standalone sibling include dirs (oneTBB): `pxr` headers pull `<tbb/...>`.
    for prefix in sibling_prefixes {
        cmd.arg(format!("-I{}", prefix.join("include").display()));
    }
    for src in sources {
        cmd.arg(src);
    }
    cmd.arg(format!("-L{}", usd_lib.display())).arg("-lusd_ms");
    // Standalone: oneTBB lives in a sibling prefix's `lib/`; the monolith's TBB
    // dependency + the exec inline templates need it linked explicitly.
    for prefix in sibling_prefixes {
        cmd.arg(format!("-L{}", prefix.join("lib").display()));
    }
    if target_os == "android" || target_os == "visionos" {
        cmd.arg("-ltbb");
    }
    if apple {
        // `@rpath` install name + lazy `dynamic_lookup`: the plugin's *own* USD
        // symbols bind from the already-loaded `usd_ms` image at load (the editor
        // on host, the runtime on device), so it carries no rpath of its own. How
        // the plugin itself is found differs by consumer: on host/desktop it is
        // `dlopen`ed by full path (install name irrelevant); on visionOS it is a
        // forced `LC_LOAD_DYLIB @rpath/libidtx_usd_exec.dylib` of the runtime
        // cdylib, which the packager resolves by staging this dylib in the
        // bundle's `Frameworks/` (the dir its `@rpath` points at) — see
        // `stage_exec_plugin_visionos` in `idtx-build`.
        cmd.arg("-install_name")
            .arg("@rpath/libidtx_usd_exec.dylib")
            .arg("-undefined")
            .arg("dynamic_lookup");
    } else {
        // Linux host + Android: undefined USD symbols bind from the already-loaded
        // host image at dlopen. No build-host rpath (meaningless on device). Set a
        // bare `DT_SONAME` so a consumer's `DT_NEEDED` records `libidtx_usd_exec.so`
        // (resolved from the APK native-lib dir at runtime), not a build path.
        cmd.arg("-Wl,--allow-shlib-undefined");
        cmd.arg(format!("-Wl,-soname,{name}"));
    }
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke {compiler}: {e}"));
    assert!(status.success(), "{name} link failed (exit {status})");
    output
}

/// Read a target-scoped build env var (`<base>_<triple>`), trying the exact triple
/// (dashes, as the runtime cross-builds emit), then the underscored form, then the
/// bare `<base>`. Mirrors the `cc` crate's lookup order so this build script picks
/// up the identical `CXX_<triple>` / `CXXFLAGS_<triple>` the cxx bridge consumes.
fn env_for_target(base: &str, target: &str) -> Option<String> {
    if !target.is_empty() {
        println!("cargo:rerun-if-env-changed={base}_{target}");
        if let Ok(v) = env::var(format!("{base}_{target}")) {
            return Some(v);
        }
        let underscored = target.replace('-', "_");
        println!("cargo:rerun-if-env-changed={base}_{underscored}");
        if let Ok(v) = env::var(format!("{base}_{underscored}")) {
            return Some(v);
        }
    }
    println!("cargo:rerun-if-env-changed={base}");
    env::var(base).ok()
}

/// Copy `generatedSchema.usda` next to the DLL and write `plugInfo.json` with its
/// `@PLUG_INFO_*@` placeholders substituted for the flat baked layout. Unlike the
/// codeless extension (empty `LibraryPath`), the coded plugin's `LibraryPath` is
/// the built DLL's filename so `PlugRegistry` dlopens it.
fn stage_resources(manifest: &Path, plugin_dir: &Path, lib_path: &Path) {
    let gen_src = manifest.join("resources").join("generatedSchema.usda");
    fs::copy(&gen_src, plugin_dir.join("generatedSchema.usda"))
        .unwrap_or_else(|e| panic!("copy generatedSchema.usda: {e}"));

    let lib_name = lib_path
        .file_name()
        .expect("plugin library has a filename")
        .to_string_lossy()
        .into_owned();
    let plug_src = manifest.join("resources").join("plugInfo.json");
    let plug = fs::read_to_string(&plug_src)
        .unwrap_or_else(|e| panic!("read {}: {e}", plug_src.display()))
        .replace("@PLUG_INFO_ROOT@", ".")
        .replace("@PLUG_INFO_RESOURCE_PATH@", ".")
        .replace("@PLUG_INFO_LIBRARY_PATH@", &lib_name);
    // Strip `#` comment lines: OpenUSD's JS parser tolerates them, but a plain
    // JSON reader does not, and a leading `#` header has caused plugin-load
    // issues before (see the `collision_schema_pluginfo_regen_drift` note). The
    // readable comments stay in the source `resources/plugInfo.json`.
    let plug = strip_hash_comments(&plug);
    fs::write(plugin_dir.join("plugInfo.json"), plug)
        .unwrap_or_else(|e| panic!("write plugInfo.json: {e}"));
}

/// Drop lines whose first non-whitespace character is `#` (whole-line comments).
/// Does not touch `#` inside string values (none exist in our plugInfo).
fn strip_hash_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}
