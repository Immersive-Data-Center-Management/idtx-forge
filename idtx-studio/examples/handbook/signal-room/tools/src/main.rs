//! Developer helper; the actual experience is the USD and single-file behaviors.
use std::error::Error;
use std::path::{Path, PathBuf};

// Native compilation checks the same source; native SDK host calls are inert.
#[allow(dead_code)]
#[path = "../../behaviors/console.rs"]
mod console;
#[allow(dead_code)]
#[path = "../../behaviors/pointer.rs"]
mod pointer;
#[allow(dead_code)]
#[path = "../../extensions/spawn_demo.rs"]
mod spawn_demo;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "check".into());
    let root = if let Some(flag) = args.next() {
        if flag != "--project-root" { return Err("expected --project-root PATH".into()); }
        PathBuf::from(args.next().ok_or("--project-root requires a path")?).canonicalize()?
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().canonicalize()?
    };
    if args.next().is_some() { return Err("unexpected trailing argument".into()); }
    check_project(&root)?;
    match command.as_str() {
        "check" => {}
        "compile" => compile_behaviors(&root)?,
        "prepare-smoke" => {
            compile_behaviors(&root)?;
            prepare_smoke_stages(&root)?;
        }
        "usd" => {
            #[cfg(feature = "usd")]
            check_usd(&root)?;
            #[cfg(not(feature = "usd"))]
            return Err("USD check requires --features usd".into());
        }
        _ => return Err("usage: signal-room-tools [check|compile|prepare-smoke|usd] [--project-root PATH]".into()),
    }
    Ok(())
}

fn prepare_smoke_stages(root: &Path) -> Result<(), Box<dyn Error>> {
    // Generated wrappers stamp modules for a bare-stage headless smoke run.
    // They do not claim to represent Conductor's complete cooked project.
    for level in ["hub", "lab"] {
        let text = format!(r#"#usda 1.0
(
    defaultPrim = "World"
    metersPerUnit = 1
    upAxis = "Y"
)
def Xform "World" (prepend references = @../levels/{level}.usda@</World>)
{{
    # Smoke-only local-role marker selects the stationary authored camera.
    custom token idtx:playerRole = "local"
    over "ViewCam"
    {{
        double3 xformOp:translate = (0, 1.5, 0)
    }}
    over "Console"
    {{
        asset idtx:behavior:module = @./behaviors/console.wasm@
    }}
    over "Pointer"
    {{
        asset idtx:behavior:module = @./behaviors/pointer.wasm@
    }}
}}
"#);
        let file = root.join(format!("build/smoke-{level}.usda"));
        std::fs::write(&file, text)?;
        println!("Prepared smoke wrapper: {}", file.display());
    }
    Ok(())
}

fn check_project(root: &Path) -> Result<(), Box<dyn Error>> {
    let project = idtx_project::IdtxProject::load(root.join("project.toml"))?;
    for level in project.levels_abs() {
        if !level.is_file() { return Err(format!("missing registered level: {}", level.display()).into()); }
    }
    let map = std::fs::read_to_string(root.join("input_map.toml"))?;
    idtx_behavior::InputMap::parse(&map)?;
    for file in ["assets/station.usda", "behaviors/console.rs", "behaviors/pointer.rs"] {
        if !root.join(file).is_file() { return Err(format!("missing project file: {file}").into()); }
    }
    println!("Project manifest, registered level files and input map: OK");
    Ok(())
}

fn compile_behaviors(root: &Path) -> Result<(), Box<dyn Error>> {
    let sdk = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../crates/idtx-behavior-sdk").canonicalize()?;
    for (name, source) in [("console", "behaviors/console.rs"), ("pointer", "behaviors/pointer.rs"), ("spawn_demo", "extensions/spawn_demo.rs")] {
        let options = idtx_behavior_build::BuildOptions::new(root.join("build/behaviors"), &sdk);
        let mut options = options;
        options.name = Some(name.into());
        let result = idtx_behavior_build::build_behavior(&root.join(source), &options)?;
        println!("Compiled {} (cache hit: {})", result.wasm_path.display(), result.cache_hit);
    }
    println!("These artifacts are for compilation checks. Conductor compiles and stamps module paths into its own preview/cook layers.");
    Ok(())
}

#[cfg(feature = "usd")]
fn check_usd(root: &Path) -> Result<(), Box<dyn Error>> {
    idtx_usd_extension::install()?;
    for (level, target, speed) in [("hub", "lab", 1.5), ("lab", "hub", 0.75)] {
        let file = root.join(format!("levels/{level}.usda"));
        let kind = idtx_usd::read_stage_kind(&file)?;
        assert_eq!(kind.stage_kind, "level");
        let stage = idtx_usd::StageService::open(&file)?;
        let panel = stage.prim_at_path("/World/Console").ok_or("missing composed console")?;
        assert!(stage.prim_has_api(panel, "IdtxBehaviorAPI")?);
        for path in ["/World/Beacon", "/World/Pointer", "/World/Spawn", "/World/Console/Toggle", "/World/Console/Travel"] {
            if stage.prim_at_path(path).is_none() { return Err(format!("missing composed prim: {path}").into()); }
        }
        let context = stage.authored_string(panel, "idtx:behavior:context", 256)?.ok_or("missing authored context")?;
        let config: console::ConsoleConfig = idtx_behavior_sdk::context::decode(context.as_bytes()).map_err(|e| e.to_string())?;
        assert_eq!(config.target_level.as_str(), target);
        assert_eq!(config.speed_radians, speed);
        println!("OpenUSD composition, schema and typed context: {level} OK");
    }
    Ok(())
}

