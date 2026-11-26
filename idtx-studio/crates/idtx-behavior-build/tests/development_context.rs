//! Exercise the actual Cargo context against the shipped SDK: live paths,
//! diagnostics, source/snapshot parity, stability and repair.

use std::path::Path;
use std::process::{Command, Output};

use idtx_behavior_build::{BuildOptions, DevelopmentContext, DevelopmentOptions, build_behavior, check_behavior, prepare_development, stage_sdk};

fn check(context: &DevelopmentContext, target: &Path) -> Output {
    let command = context.check_command(target).unwrap();
    Command::new(&command[0]).args(&command[1..])
        .current_dir(context.manifest_path.parent().unwrap())
        .envs(&context.environment).output().unwrap()
}

fn assert_checked(output: &Output) {
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stderr), String::from_utf8_lossy(&output.stdout));
}

#[test]
fn live_context_uses_original_modules_and_is_stable_and_repairable_even_with_broken_code() {
    let temp = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    let project = temp.path().join("author 'é\" project");
    #[cfg(windows)]
    let project = temp.path().join("author 'é project");
    let entry = project.join("behaviors/spin/behavior.rs");
    let helper = project.join("behaviors/spin/motion.rs");
    let shared = project.join("behaviors/shared/lib.rs");
    std::fs::create_dir_all(helper.parent().unwrap().join("motion")).unwrap();
    std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
    let source = r#"//! Author-owned module body.
use idtx_behavior_sdk::prelude::*;
mod motion;
#[path = "../shared/lib.rs"]
mod shared;
#[derive(Default)]
pub struct Spin;
#[behavior(kind = "runtime")]
impl Spin {
    fn on_update(&mut self, ctx: &Ctx, dt: f32) -> Result<()> {
        let _ = (ctx.scene(), motion::step(dt) + shared::OFFSET);
        Ok(())
    }
}
"#;
    std::fs::write(&entry, source).unwrap();
    let helper_source = "mod easing;\npub fn step(dt: f32) -> f32 { easing::smooth(dt) }\n";
    std::fs::write(&helper, helper_source).unwrap();
    std::fs::write(helper.parent().unwrap().join("motion/easing.rs"), "pub fn smooth(dt: f32) -> f32 { dt * dt }").unwrap();
    std::fs::write(&shared, "pub const OFFSET: f32 = 0.5;").unwrap();
    let sdk = temp.path().join("shipped sdk");
    stage_sdk(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap(), &sdk).unwrap();
    let mut options = DevelopmentOptions::new(project.join(".idtx/behavior-ide"), sdk.join("idtx-behavior-sdk"));
    let context = prepare_development(&entry, &options).unwrap();
    let manifest = std::fs::read(&context.manifest_path).unwrap();
    let modified = std::fs::metadata(&context.manifest_path).unwrap().modified().unwrap();
    let wrapper_path = context.manifest_path.parent().unwrap().join("src/lib.rs");
    let wrapper = std::fs::read_to_string(&wrapper_path).unwrap();
    assert!(wrapper.contains("#[path = "));
    assert!(wrapper.contains(&format!("{:?}", context.source.entry().to_str().unwrap())));
    assert!(!wrapper_path.with_file_name("behavior.rs").exists(), "there must not be an editable shadow source");
    assert!(context.manifest_path.with_file_name("Cargo.lock").is_file());
    let target = options.context_dir.join("target-ra");
    assert_checked(&check(&context, &target));

    // Cargo diagnostics must target the helper the author actually edits, and
    // preparation must keep working while that helper has a type error.
    std::fs::write(&helper, "pub fn step(_: f32) -> f32 { \"wrong type\" }\n").unwrap();
    let second = prepare_development(&entry, &options).unwrap();
    assert_eq!(second.manifest_path, context.manifest_path);
    assert_eq!(std::fs::metadata(&second.manifest_path).unwrap().modified().unwrap(), modified);
    let output = check(&second, &target);
    assert!(!output.status.success());
    let messages: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout).lines()
        .filter_map(|line| serde_json::from_str(line).ok()).collect();
    let actual_helper = context.source.entry().with_file_name("motion.rs");
    assert!(messages.iter().any(|m| m["message"]["code"]["code"] == "E0308"
        && m["message"]["spans"].as_array().is_some_and(|spans| spans.iter().any(|s|
            s["file_name"].as_str().is_some_and(|p| Path::new(p) == actual_helper)))), "{messages:#?}");
    let build_opts = BuildOptions::new(project.join("behaviors"), &options.sdk_path);
    let error = check_behavior(&entry, &build_opts).unwrap_err().to_string();
    assert!(error.contains("motion.rs"), "{error}");
    assert!(!error.contains("src/authored/"), "{error}");

    // A new module is discovered through normal Rust rules without regenerating.
    std::fs::write(&helper, format!("{helper_source}\nmod added;\n")).unwrap();
    std::fs::write(helper.parent().unwrap().join("motion/added.rs"), "const _: &str = file!(); const _: &str = env!(\"CARGO_MANIFEST_DIR\");").unwrap();
    assert_checked(&check(&second, &target));
    let artifact = build_behavior(&entry, &build_opts).unwrap();
    let bytes = std::fs::read(&artifact.wasm_path).unwrap();
    assert!(idtx_behavior::verify_link(&bytes, idtx_behavior::Capabilities::all()).unwrap().links);

    // Editor manifests cannot become an authoritative production build recipe.
    std::fs::write(&context.manifest_path, "intentionally damaged generated manifest").unwrap();
    std::fs::write(context.manifest_path.with_file_name("Cargo.lock"), "damaged lockfile").unwrap();
    assert!(build_behavior(&entry, &build_opts).unwrap().cache_hit);
    options.regenerate = true;
    let repaired = prepare_development(&entry, &options).unwrap();
    assert_eq!(repaired.manifest_path, context.manifest_path);
    assert_eq!(std::fs::read(&repaired.manifest_path).unwrap(), manifest);
    assert_eq!(std::fs::read_to_string(&entry).unwrap(), source);
    assert_checked(&check(&repaired, &target));
}
