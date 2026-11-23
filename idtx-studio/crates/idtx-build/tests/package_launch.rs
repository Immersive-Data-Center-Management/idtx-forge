//! Real binary + cooked payload + native libraries, launched without a USD env.
use std::{fs, path::PathBuf, process::{Command, Stdio}, time::{Duration, Instant}};
use idtx_build::{cook, CookPlan, CookTarget, RuntimeBinary};

#[path = "../../../tests/support/mod.rs"]
mod support;

#[test]
#[ignore = "requires IDTX_TEST_RUNTIME built with metrics and OpenUSD 26.11.1"]
fn packaged_native_runtime_discovers_payload_and_renders_after_relocation() {
    let runtime = PathBuf::from(std::env::var_os("IDTX_TEST_RUNTIME").expect("IDTX_TEST_RUNTIME is required")).canonicalize().unwrap();
    let scratch = support::scratch_dir("package-launch");
    let root = scratch.path().canonicalize().unwrap();
    fs::write(root.join("level.usda"), "#usda 1.0\n(defaultPrim = \"World\")\ndef Xform \"World\"\n{\n    def Sphere \"Ball\"\n    {\n        double radius = 1\n    }\n}\n").unwrap();
    let mut manifest = idtx_project::ProjectManifest::new("LaunchProbe");
    manifest.default_stage = Some("level.usda".into());
    let project = idtx_project::IdtxProject::create(&root, manifest).unwrap();
    let rustc = Command::new("rustc").arg("-vV").output().unwrap();
    let rustc = String::from_utf8(rustc.stdout).unwrap();
    let triple = rustc.lines().find_map(|s| s.strip_prefix("host: ")).unwrap();
    let target = CookTarget::desktop(triple);
    let mut plan = CookPlan::from_project(&project, target.clone(), root.join("target/cooked")).unwrap();
    plan.player = None; plan.xr.controllers.clear();
    let cooked = cook(&plan).unwrap();
    let binary = RuntimeBinary::new(&runtime);
    #[cfg(target_os = "macos")]
    let (bundle, relative_exe) = {
        let info = idtx_build::MacAppInfo { name: "ReviewPlayer".into(), identifier: "com.idtx.review.launch".into(), executable: "ReviewPlayer".into() };
        let bundle = idtx_build::MacAppPackager::new(target, binary, root.join("target/package"), info)
            .package_with(&cooked, |app| {
                let frameworks = app.join("Contents/Frameworks");
                assert!(idtx_build::stage_usd_runtime(&frameworks, runtime.parent().unwrap())?.monolithic_found);
                let exe = app.join("Contents/MacOS/ReviewPlayer");
                let install = PathBuf::from(std::env::var_os("IDTX_USD_DIR").unwrap());
                idtx_build::delete_rpath(&exe, install.join("lib").to_str().unwrap()).unwrap();
                idtx_build::add_rpath(&exe, "@executable_path/../Frameworks").unwrap();
                for entry in fs::read_dir(frameworks).unwrap().flatten() {
                    if entry.path().extension().is_some_and(|s| s == "dylib") {
                        idtx_build::add_rpath(&entry.path(), "@loader_path").unwrap();
                        idtx_build::codesign_adhoc(&entry.path(), false).unwrap();
                    }
                }
                // Sign the standalone executable outside Contents/MacOS:
                // codesign otherwise treats the unsealed data-bearing app as
                // a bundle. Full app signing is a separate packaging contract.
                let signing = app.parent().unwrap().join("runtime-sign");
                fs::copy(&exe, &signing).unwrap();
                idtx_build::codesign_adhoc(&signing, false).unwrap();
                fs::copy(&signing, &exe).unwrap();
                fs::remove_file(signing).unwrap();
                Ok(())
            }).unwrap();
        (bundle, PathBuf::from("Contents/MacOS/ReviewPlayer"))
    };
    #[cfg(not(target_os = "macos"))]
    let (bundle, relative_exe) = {
        let bundle = idtx_build::DesktopFolderPackager::new(target, binary, root.join("target/package"))
            .with_bundle_name("ReviewPlayer").package_with(&cooked, |bundle| {
                assert!(idtx_build::stage_usd_runtime(bundle, runtime.parent().unwrap())?.monolithic_found); Ok(())
            }).unwrap();
        (bundle, PathBuf::from(runtime.file_name().unwrap()))
    };
    let moved = root.join("relocated");
    fs::rename(bundle, &moved).unwrap();
    let cwd = root.join("unrelated-cwd"); fs::create_dir(&cwd).unwrap();
    let screenshot = root.join("render.png");
    let log = root.join("launch.log");
    let file = fs::File::create(&log).unwrap();
    let mut command = Command::new(moved.join(relative_exe));
    command.current_dir(cwd).args(["--headless", "--frames", "3", "--screenshot"]).arg(&screenshot)
        .env_remove("IDTX_USD_DIR").env_remove("DYLD_LIBRARY_PATH").env_remove("PXR_PLUGINPATH_NAME")
        .stdout(Stdio::from(file.try_clone().unwrap())).stderr(Stdio::from(file));
    #[cfg(target_os = "macos")]
    command.env("DYLD_PRINT_LIBRARIES", "1");
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() { break status; }
        if Instant::now() >= deadline { let _ = child.kill(); panic!("packaged runtime timed out: {}", fs::read_to_string(&log).unwrap()); }
        std::thread::sleep(Duration::from_millis(20));
    };
    let log = fs::read_to_string(log).unwrap();
    assert!(status.success(), "packaged launch failed: {log}");
    let image = fs::read(screenshot).expect("packaged renderer produced a screenshot");
    assert!(image.starts_with(b"\x89PNG\r\n\x1a\n") && image.len() > 64);
    #[cfg(target_os = "macos")]
    assert!(log.contains(&moved.join("Contents/Frameworks/libusd_ms.dylib").to_string_lossy().to_string()), "launch did not resolve the bundled USD: {log}");
}
