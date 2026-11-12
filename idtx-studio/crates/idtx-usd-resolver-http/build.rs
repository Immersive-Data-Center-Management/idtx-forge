use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=IDTX_USD_DIR");
    println!("cargo:rerun-if-env-changed=IDTX_USD_DIR_ANDROID");
    println!("cargo:rerun-if-env-changed=IDTX_USD_DIR_VISIONOS");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=cpp/IdtxHttpArResolver.cpp");
    println!("cargo:rerun-if-changed=plugInfo.json");

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    // In a build script `cfg!(target_os = …)` is the HOST, not the compilation
    // target — a landmine when cross-compiling (e.g. a macOS host building for
    // `aarch64-apple-visionos`). The real target is in `CARGO_CFG_TARGET_OS` /
    // `TARGET`; every host-vs-target decision below keys off them. Mirrors
    // `idtx-usd-sys` / `idtx-usd-exec`.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target = env::var("TARGET").unwrap_or_default();

    // Target-aware USD prefix: host → `$IDTX_USD_DIR`; android/visionos → the
    // matching cross prefix (`$IDTX_USD_DIR_VISIONOS`, …).
    let usd_dir = idtx_usd_build_support::resolve_usd_dir_for_target(&manifest, &target_os);
    let usd_include = usd_dir.join("include");
    let usd_lib = usd_dir.join("lib");

    // Android/visionOS: oneTBB lives in a SEPARATE sibling prefix (the cross-build
    // layout), and the resolver's `pxr/usd/ar` headers pull `<tbb/...>` + the
    // monolithic lib carries a oneTBB dependency. No MaterialX needed here.
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

    if target_os == "windows" {
        idtx_usd_build_support::prime_msvc_env();
    }

    let dylib_out = build_resolver_dylib(
        &manifest,
        &usd_include,
        &usd_lib,
        &sibling_prefixes,
        &out_dir,
        &target_os,
        &target,
    );

    // Emit a platform-appropriate plugInfo.json next to the dylib.
    write_plug_info(&dylib_out, &out_dir);

    // Stash the plugin directory so the Rust crate can find it at runtime.
    println!(
        "cargo:rustc-env=IDTX_HTTP_RESOLVER_PLUGIN_DIR={}",
        out_dir.display()
    );

    // Link against the resolver dylib we just built.
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=dylib=idtx_http_resolver");

    // Propagate rpath so the resolver dylib is found at `cargo run` time — host
    // builds only. Android/visionOS load it from the package's own lib dir
    // (`lib/<abi>/`, the `.app`'s `Frameworks/`), where a build-host rpath would
    // be meaningless and the install prefix does not exist.
    match target_os.as_str() {
        "macos" => {
            println!("cargo:rustc-link-arg=-rpath");
            println!("cargo:rustc-link-arg={}", out_dir.display());
            println!("cargo:rustc-link-arg=-rpath");
            println!("cargo:rustc-link-arg={}", usd_lib.display());
        }
        "linux" => {
            println!(
                "cargo:rustc-link-arg=-Wl,-rpath,{}:{}",
                out_dir.display(),
                usd_lib.display()
            );
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)] // a build-script helper threading resolved paths + target info
fn build_resolver_dylib(
    manifest: &Path,
    usd_include: &Path,
    usd_lib: &Path,
    sibling_prefixes: &[PathBuf],
    out_dir: &Path,
    target_os: &str,
    target: &str,
) -> PathBuf {
    let src = manifest.join("cpp/IdtxHttpArResolver.cpp");

    if target_os == "windows" {
        build_resolver_dll_msvc(&src, usd_include, usd_lib, out_dir)
    } else {
        build_resolver_dylib_unix(
            &src,
            usd_include,
            usd_lib,
            sibling_prefixes,
            out_dir,
            target_os,
            target,
        )
    }
}

/// Compiles the resolver as a Windows DLL using cl.exe + link.exe (MSVC toolchain).
///
/// `prime_msvc_env()` must have been called before this so that cl.exe and
/// link.exe are on PATH.
fn build_resolver_dll_msvc(
    src: &Path,
    usd_include: &Path,
    usd_lib: &Path,
    out_dir: &Path,
) -> PathBuf {
    let obj = out_dir.join("IdtxHttpArResolver.obj");
    let output = out_dir.join("idtx_http_resolver.dll");
    // Explicit /IMPLIB ensures the import library lands in out_dir alongside
    // the DLL, matching the rustc-link-search=native path we emit below.
    let implib = out_dir.join("idtx_http_resolver.lib");

    // Compile to object file with MSVC flags.
    let status = Command::new("cl.exe")
        .arg("/c")
        .arg("/std:c++17")
        .arg("/O2")
        .arg("/MD") // dynamic CRT, matches the USD monolithic build
        .arg("/EHsc") // C++ exception handling
        .arg("/wd4996") // suppress deprecated-declaration warnings
        .arg(format!("/I{}", usd_include.display()))
        .arg(format!("/Fo:{}", obj.display()))
        .arg(src)
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke cl.exe: {e}"));
    assert!(
        status.success(),
        "cl.exe compilation of IdtxHttpArResolver.cpp failed"
    );

    // Link the object into a DLL.
    let status = Command::new("link.exe")
        .arg("/DLL")
        .arg("/NOLOGO")
        .arg(format!("/OUT:{}", output.display()))
        .arg(format!("/IMPLIB:{}", implib.display()))
        .arg(format!("/LIBPATH:{}", usd_lib.display()))
        .arg("usd_ms.lib")
        .arg(&obj)
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke link.exe: {e}"));
    assert!(
        status.success(),
        "link.exe DLL linking of IdtxHttpArResolver failed"
    );

    output
}

/// Compiles the resolver as a shared library on macOS / Linux (host) **and** the
/// Android / visionOS cross paths, using a clang-like driver.
///
/// # Cross-compilation
///
/// The compiler and its target flags come from the same target-scoped env vars
/// the runtime cross-builds (`cargo xtask build-quest-runtime` /
/// `build-visionos-runtime`) already set for the cxx bridge:
/// * `CXX_<triple>` — the NDK clang++ (Android, already embeds `--target` +
///   sysroot) or the `xcrun clang++` (visionOS). Falls back to `CXX`, then
///   `clang++`.
/// * `CXXFLAGS_<triple>` — the visionOS `-isysroot <sdk> -target arm64-apple-xros…`
///   (empty for Android, whose clang wrapper carries them).
///
/// Without these the driver defaults to the host (macOS) SDK and target, so a
/// visionOS build picks `pthread_cond_timedwait` from the macOS SDK and fails
/// with "unavailable: not available on visionOS".
#[allow(clippy::too_many_arguments)] // a build-script helper threading resolved paths + target info
fn build_resolver_dylib_unix(
    src: &Path,
    usd_include: &Path,
    usd_lib: &Path,
    sibling_prefixes: &[PathBuf],
    out_dir: &Path,
    target_os: &str,
    target: &str,
) -> PathBuf {
    // macOS + visionOS ship a Mach-O `.dylib`; linux + android an ELF `.so`.
    let apple = target_os == "macos" || target_os == "visionos";
    let (dylib_name, shared_flag) = if apple {
        ("libidtx_http_resolver.dylib", "-dynamiclib")
    } else {
        ("libidtx_http_resolver.so", "-shared")
    };

    let output = out_dir.join(dylib_name);

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
        .arg("-Wno-deprecated-declarations")
        .arg("-o")
        .arg(&output)
        .arg(src)
        .arg(format!("-I{}", usd_include.display()));
    // Standalone sibling include dirs (oneTBB): `pxr/usd/ar` headers pull `<tbb/...>`.
    for prefix in sibling_prefixes {
        cmd.arg(format!("-I{}", prefix.join("include").display()));
    }
    cmd.arg(format!("-L{}", usd_lib.display())).arg("-lusd_ms");
    // Standalone: oneTBB lives in a sibling prefix's `lib/`; link it explicitly.
    for prefix in sibling_prefixes {
        cmd.arg(format!("-L{}", prefix.join("lib").display()));
    }
    if target_os == "android" || target_os == "visionos" {
        cmd.arg("-ltbb");
    }

    if apple {
        // `@rpath` install name so the dylib is relocatable, + lazy
        // `dynamic_lookup` so USD symbols bind from the already-loaded `usd_ms`
        // image at load. On visionOS the packager stages this dylib in the
        // bundle's `Frameworks/idtx-http-resolver/` (the dir `resolve_plugin_dir`
        // points at); on host it is loaded by full path.
        cmd.arg("-install_name")
            .arg("@rpath/libidtx_http_resolver.dylib")
            .arg("-undefined")
            .arg("dynamic_lookup");
    } else {
        // Linux host + Android: undefined USD symbols bind from the already-loaded
        // image at dlopen; no build-host rpath (meaningless on device).
        cmd.arg("-Wl,--allow-shlib-undefined");
        cmd.arg(format!("-Wl,-soname,{dylib_name}"));
    }

    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke {compiler}: {e}"));
    assert!(
        status.success(),
        "IdtxHttpArResolver.cpp compilation failed (exit {status})"
    );

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

/// Writes a `plugInfo.json` to `out_dir` with an absolute `LibraryPath`
/// pointing to the built dylib/DLL. The absolute path is required because
/// `PXR_PLUGINPATH_NAME` can be set to any directory.
fn write_plug_info(dylib_path: &Path, out_dir: &Path) {
    let lib_name = dylib_path.file_name().unwrap().to_string_lossy();

    let content = format!(
        r#"{{
  "Plugins": [{{
    "Info": {{
      "Types": {{
        "IdtxHttpArResolver": {{
          "bases": ["ArResolver"],
          "uriSchemes": ["http", "https"],
          "implementsContexts": false
        }}
      }}
    }},
    "Type": "library",
    "Name": "idtxHttpResolver",
    "LibraryPath": "{lib_name}",
    "Root": "."
  }}]
}}
"#
    );

    std::fs::write(out_dir.join("plugInfo.json"), content).expect("write plugInfo.json to OUT_DIR");
}
