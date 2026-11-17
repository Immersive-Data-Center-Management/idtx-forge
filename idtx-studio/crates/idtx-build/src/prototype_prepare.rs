//! Compile and preserve explicitly registered USD spawn templates.
use idtx_behavior_build::{BuildOptions, build_behavior};
use idtx_project::{IdtxProject, ProjectRoot, RuntimePrototypeConfig};
use idtx_usd::StageService;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const API: &str = "IdtxBehaviorAPI";
const SOURCE: &str = "idtx:behavior:source";
const MODULE: &str = "idtx:behavior:module";

#[derive(Default)]
struct Node {
    module: Option<String>,
    children: BTreeMap<String, Node>,
}

fn content_key(bytes: &[u8]) -> String {
    let mut key = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(key, "{byte:02x}");
    }
    key
}

fn asset_path(path: &Path) -> Result<String, String> {
    let value = idtx_project::strip_verbatim_prefix(path)
        .to_string_lossy()
        .replace('\\', "/");
    if value.contains(['@', '\n', '\r']) {
        return Err("USD asset path contains an unrepresentable character".into());
    }
    Ok(value)
}

fn write_children(node: &Node, output: &mut String, depth: usize) {
    let indent = "    ".repeat(depth);
    if let Some(module) = &node.module {
        let _ = writeln!(output, "{indent}asset {MODULE} = @{module}@");
        // A runtime artifact must not depend on design-time Rust source files.
        let _ = writeln!(output, "{indent}asset {SOURCE} = @@");
    }
    for (name, child) in &node.children {
        let _ = writeln!(output, "{indent}over {name:?}\n{indent}{{");
        write_children(child, output, depth + 1);
        let _ = writeln!(output, "{indent}}}");
    }
}

/// Compile every registered template independent of live stage topology and
/// return absolute paths to generated, flattened template assets. Source assets
/// are read-only. The caller owns the output directory's lifetime.
pub fn prepare_runtime_prototypes(
    project: &IdtxProject,
    sdk: Option<&Path>,
    output: &Path,
) -> Result<BTreeMap<String, RuntimePrototypeConfig>, String> {
    project.manifest().validate_runtime_prototypes()?;
    let boundary = ProjectRoot::from_canonical(
        std::fs::canonicalize(project.root()).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let builtin = std::fs::canonicalize(idtx_assets::builtin_tree()).ok();
    let engine = wasmtime::Engine::default();
    let mut result = BTreeMap::new();
    for (id, template) in &project.manifest().runtime_prototypes {
        let source = boundary
            .resolve_relative_checked(&template.asset)
            .map_err(|e| format!("prototype {id}: {e}"))?;
        if !source.is_file() {
            return Err(format!(
                "prototype {id}: asset does not exist: {}",
                idtx_project::normalize_display_path(&source)
            ));
        }
        let mut stage = StageService::open(&source).map_err(|e| format!("prototype {id}: {e}"))?;
        let prims: Vec<_> = stage
            .project_topology()
            .map_err(|e| e.to_string())?
            .iter()
            .map(|row| row.prim)
            .collect();
        if stage.prim_at_path(&template.prim).is_none() {
            return Err(format!(
                "prototype {id}: target prim {} is missing or dormant",
                template.prim
            ));
        }
        let directory = output.join(id);
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let mut root = Node::default();
        for prim in prims {
            let path = stage
                .path_for(prim)
                .ok_or("prototype prim has no path")?
                .to_owned();
            if path != template.prim && !path.starts_with(&format!("{}/", template.prim)) {
                continue;
            }
            let properties = stage.inspect_prim(prim).map_err(|e| e.to_string())?;
            let has_source = properties.iter().any(|p| {
                p.name == SOURCE && p.authored && !matches!(p.value_summary.trim(), "" | "@@")
            });
            let has_module = properties.iter().any(|p| p.name == MODULE && p.authored);
            let has_api = stage.prim_has_api(prim, API).map_err(|e| e.to_string())?;
            if !has_api && !has_source && !has_module {
                continue;
            }
            if !has_api {
                return Err(format!("prototype {id}: behavior on {path} lacks {API}"));
            }
            let wasm = if has_source {
                let resolved = stage
                    .resolve_prim_asset(prim, SOURCE)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| {
                        format!("prototype {id}: behavior source on {path} is unresolved")
                    })?;
                let authored = std::fs::canonicalize(&resolved)
                    .map_err(|e| format!("prototype {id}: source {resolved}: {e}"))?;
                if !authored.starts_with(project.root())
                    && !builtin
                        .as_ref()
                        .is_some_and(|root| authored.starts_with(root))
                {
                    return Err(format!(
                        "prototype {id}: behavior source escapes the project/built-in roots"
                    ));
                }
                let sdk = sdk.ok_or_else(|| {
                    format!("prototype {id}: behavior SDK is required to compile {path}")
                })?;
                let key = content_key(authored.as_os_str().as_encoded_bytes());
                let mut options = BuildOptions::new(output.join("modules"), sdk);
                options.name = Some(format!("template_{}", &key[..16]));
                // Hand the compiler a plain path (verbatim `\\?\` prefix stripped);
                // `authored` stays verbatim above for the stable content-key hash
                // and the in-project `starts_with` confinement checks.
                build_behavior(&idtx_project::strip_verbatim_prefix(&authored), &options)
                    .map_err(|e| format!("prototype {id}: {e}"))?
                    .wasm_path
            } else if has_module {
                let resolved = stage
                    .resolve_prim_asset(prim, MODULE)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| {
                        format!("prototype {id}: behavior module on {path} is unresolved")
                    })?;
                let bytes = stage.read_asset(&resolved).map_err(|e| e.to_string())?;
                let key = content_key(&bytes);
                let modules = output.join("modules");
                std::fs::create_dir_all(&modules).map_err(|e| e.to_string())?;
                let wasm = modules.join(format!("module_{}.wasm", &key[..16]));
                std::fs::write(&wasm, bytes).map_err(|e| e.to_string())?;
                wasm
            } else {
                return Err(format!(
                    "prototype {id}: behavior on {path} has neither source nor module"
                ));
            };
            validate_module(&engine, &std::fs::read(&wasm).map_err(|e| e.to_string())?)
                .map_err(|e| format!("prototype {id}: invalid WASM on {path}: {e}"))?;
            let mut node = &mut root;
            for part in path
                .strip_prefix(&template.prim)
                .unwrap_or_default()
                .trim_start_matches('/')
                .split('/')
                .filter(|p| !p.is_empty())
            {
                node = node.children.entry(part.to_owned()).or_default();
            }
            node.module = Some(asset_path(
                &std::fs::canonicalize(wasm).map_err(|e| e.to_string())?,
            )?);
        }
        let name = template
            .prim
            .rsplit('/')
            .next()
            .ok_or("prototype has no prim name")?;
        let prepared_prim = format!("/{name}");
        let wrapper = directory.join("wrapper.usda");
        let mut text = format!(
            "#usda 1.0\n( defaultPrim = {name:?} )\ndef {name:?} ( prepend references = @{}@<{}> )\n{{\n",
            asset_path(&source)?,
            template.prim
        );
        write_children(&root, &mut text, 1);
        text.push_str("}\n");
        std::fs::write(&wrapper, text).map_err(|e| e.to_string())?;
        let prepared = directory.join("prepared.usda");
        let wrapper_stage = StageService::open(&wrapper).map_err(|e| e.to_string())?;
        // Flattening drops unresolved arcs. Reject composition errors first so
        // a missing dependency cannot produce a deceptively valid template.
        wrapper_stage
            .validate_runtime_readiness()
            .map_err(|e| format!("prototype {id}: {e}"))?;
        wrapper_stage
            .export_flattened_stage_without_provenance(&prepared)
            .map_err(|e| e.to_string())?;
        validate_prepared_template(&prepared, &prepared_prim)?;
        result.insert(
            id.clone(),
            RuntimePrototypeConfig {
                asset: std::fs::canonicalize(prepared).map_err(|e| e.to_string())?,
                prim: prepared_prim,
            },
        );
    }
    Ok(result)
}

/// Package prepared templates with their complete localized dependency graph.
/// Registry paths are bundle-relative and remain valid when the output moves.
pub fn package_runtime_prototypes(
    prepared: &BTreeMap<String, RuntimePrototypeConfig>,
    output: &Path,
) -> Result<BTreeMap<String, RuntimePrototypeConfig>, String> {
    let directory = output.join("runtime_prototypes");
    if !prepared.is_empty() {
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    }
    let mut registry = BTreeMap::new();
    for (id, template) in prepared {
        template.validate(id)?;
        let relative = PathBuf::from("runtime_prototypes").join(format!("{id}.usdz"));
        let destination = output.join(&relative);
        idtx_usd::package_stage(&template.asset, &destination)
            .map_err(|e| format!("prototype {id}: package: {e}"))?;
        crate::usdz_metadata::normalize(&destination)
            .map_err(|e| format!("prototype {id}: package metadata: {e}"))?;
        validate_prepared_template(&destination, &template.prim)?;
        registry.insert(
            id.clone(),
            RuntimePrototypeConfig {
                asset: relative,
                prim: template.prim.clone(),
            },
        );
    }
    Ok(registry)
}

// Validate the export shape without executing guest code at design time.
// The host validates the returned ABI version and capabilities during its
// bounded startup probe before the live instance can become Ready.
fn validate_module(engine: &wasmtime::Engine, bytes: &[u8]) -> Result<(), String> {
    let module = wasmtime::Module::new(engine, bytes).map_err(|e| e.to_string())?;
    let export = module
        .get_export("_idtx_abi_version")
        .ok_or("missing _idtx_abi_version export")?;
    let function = export
        .func()
        .ok_or("_idtx_abi_version must be a function")?;
    if function.params().len() != 0
        || function.results().len() != 1
        || !matches!(function.results().next(), Some(wasmtime::ValType::I32))
    {
        return Err("_idtx_abi_version must have signature () -> i32".into());
    }
    Ok(())
}

fn validate_prepared_template(asset: &Path, root: &str) -> Result<(), String> {
    let mut stage = StageService::open(asset).map_err(|e| e.to_string())?;
    let prims: Vec<_> = stage
        .project_topology()
        .map_err(|e| e.to_string())?
        .iter()
        .map(|row| row.prim)
        .collect();
    if stage.prim_at_path(root).is_none() {
        return Err(format!("prepared template root {root} is missing"));
    }
    let engine = wasmtime::Engine::default();
    for prim in prims {
        if !stage.prim_has_api(prim, API).map_err(|e| e.to_string())? {
            continue;
        }
        let resolved = stage
            .resolve_prim_asset(prim, MODULE)
            .map_err(|e| e.to_string())?
            .ok_or("prepared behavior module is unresolved")?;
        let bytes = stage.read_asset(&resolved).map_err(|e| e.to_string())?;
        validate_module(&engine, &bytes)
            .map_err(|e| format!("prepared behavior is invalid: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn behavior_template_modules_survive_relocation_and_packages_are_reproducible() {
        idtx_usd_extension::install().unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let project_dir = temporary.path().join("project");
        std::fs::create_dir_all(project_dir.join("assets")).unwrap();
        let wasm = wat::parse_str(format!(
            "(module (func (export \"_idtx_abi_version\") (result i32) i32.const {}))",
            idtx_behavior::ABI_VERSION
        ))
        .unwrap();
        std::fs::write(project_dir.join("assets/panel.wasm"), &wasm).unwrap();
        std::fs::write(
            project_dir.join("assets/panel.usda"),
            r#"#usda 1.0
( defaultPrim = "Panel" )
def Xform "Panel" {
    def Xform "Behavior" ( prepend apiSchemas = ["IdtxBehaviorAPI"] ) {
        asset idtx:behavior:source = @@
        asset idtx:behavior:module = @panel.wasm@
        string idtx:behavior:uuid = "panel-fixture"
        token idtx:behavior:kind = "runtime"
        bool idtx:behavior:enabled = true
    }
}
"#,
        )
        .unwrap();
        let mut manifest = idtx_project::ProjectManifest::new("Templates");
        manifest.runtime_prototypes.insert(
            "panel".into(),
            RuntimePrototypeConfig {
                asset: "assets/panel.usda".into(),
                prim: "/Panel".into(),
            },
        );
        let project = IdtxProject::create(&project_dir, manifest).unwrap();
        let original = std::fs::read(project_dir.join("assets/panel.usda")).unwrap();
        let prepared =
            prepare_runtime_prototypes(&project, None, &temporary.path().join("prepare_a"))
                .unwrap();
        let payload = temporary.path().join("payload");
        let registry = package_runtime_prototypes(&prepared, &payload).unwrap();
        let prepared_b =
            prepare_runtime_prototypes(&project, None, &temporary.path().join("prepare_b"))
                .unwrap();
        let payload_b = temporary.path().join("payload_b");
        package_runtime_prototypes(&prepared_b, &payload_b).unwrap();
        assert_eq!(
            std::fs::read(payload.join(&registry["panel"].asset)).unwrap(),
            std::fs::read(payload_b.join(&registry["panel"].asset)).unwrap()
        );
        assert_eq!(
            std::fs::read(project_dir.join("assets/panel.usda")).unwrap(),
            original
        );
        let relocated = temporary.path().join("relocated");
        std::fs::rename(&payload, &relocated).unwrap();
        std::fs::remove_file(project_dir.join("assets/panel.wasm")).unwrap();
        let stage = StageService::open(relocated.join(&registry["panel"].asset)).unwrap();
        let prim = stage.prim_at_path("/Panel/Behavior").unwrap();
        let resolved = stage.resolve_prim_asset(prim, MODULE).unwrap().unwrap();
        assert!(resolved.contains(".usdz["), "{resolved}");
        assert_eq!(stage.read_asset(&resolved).unwrap(), wasm);
        validate_prepared_template(&relocated.join(&registry["panel"].asset), "/Panel").unwrap();
    }

    #[test]
    fn behavior_templates_reject_modules_without_the_abi_contract() {
        let engine = wasmtime::Engine::default();
        assert!(validate_module(&engine, &wat::parse_str("(module)").unwrap()).is_err());
        assert!(
            validate_module(
                &engine,
                &wat::parse_str(
                    "(module (func (export \"_idtx_abi_version\") (result i64) i64.const 3))"
                )
                .unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn missing_template_composition_dependencies_fail_before_flattening() {
        let temporary = tempfile::tempdir().unwrap();
        let project_dir = temporary.path().join("project");
        std::fs::create_dir_all(project_dir.join("assets")).unwrap();
        std::fs::write(project_dir.join("assets/panel.usda"), "#usda 1.0\n( defaultPrim = \"Panel\" )\ndef Xform \"Panel\" ( prepend references = @missing.usda@ ) {}\n").unwrap();
        let mut manifest = idtx_project::ProjectManifest::new("Templates");
        manifest.runtime_prototypes.insert(
            "panel".into(),
            RuntimePrototypeConfig {
                asset: "assets/panel.usda".into(),
                prim: "/Panel".into(),
            },
        );
        let project = IdtxProject::create(&project_dir, manifest).unwrap();
        assert!(
            prepare_runtime_prototypes(&project, None, &temporary.path().join("prepared")).is_err()
        );
    }

    #[test]
    fn direct_cook_emits_registered_templates_absent_from_the_live_stage() {
        let temporary = tempfile::tempdir().unwrap();
        let project_dir = temporary.path().join("project");
        std::fs::create_dir_all(project_dir.join("assets")).unwrap();
        std::fs::write(
            project_dir.join("world.usda"),
            "#usda 1.0\n( defaultPrim = \"World\" )\ndef Xform \"World\" {}\n",
        )
        .unwrap();
        std::fs::write(
            project_dir.join("assets/panel.usda"),
            "#usda 1.0\n( defaultPrim = \"Panel\" )\ndef Xform \"Panel\" {}\n",
        )
        .unwrap();
        let mut manifest = idtx_project::ProjectManifest::new("Templates");
        manifest.default_stage = Some("world.usda".into());
        manifest.runtime_prototypes.insert(
            "panel".into(),
            RuntimePrototypeConfig {
                asset: "assets/panel.usda".into(),
                prim: "/Panel".into(),
            },
        );
        let project = IdtxProject::create(&project_dir, manifest).unwrap();
        let payload = temporary.path().join("payload");
        let plan =
            crate::CookPlan::from_project(&project, crate::CookTarget::desktop("test"), &payload)
                .unwrap();
        let report = crate::cook(&plan).unwrap();
        let runtime = idtx_runtime_manifest::RuntimeManifest::load(&report.manifest).unwrap();
        assert_eq!(
            runtime.runtime_prototypes["panel"].asset,
            Path::new("runtime_prototypes/panel.usdz")
        );
        let relocated = temporary.path().join("relocated");
        std::fs::rename(&payload, &relocated).unwrap();
        let template = &runtime.runtime_prototypes["panel"];
        validate_prepared_template(&relocated.join(&template.asset), &template.prim).unwrap();
    }

    #[test]
    fn uninstantiated_template_packages_and_survives_relocation() {
        let temporary = tempfile::tempdir().unwrap();
        let project_dir = temporary.path().join("project");
        std::fs::create_dir_all(project_dir.join("assets")).unwrap();
        std::fs::write(
            project_dir.join("assets/panel.usda"),
            "#usda 1.0\n( defaultPrim = \"Panel\" )\ndef Xform \"Panel\" {}\n",
        )
        .unwrap();
        let mut manifest = idtx_project::ProjectManifest::new("Templates");
        manifest.runtime_prototypes.insert(
            "panel".into(),
            RuntimePrototypeConfig {
                asset: "assets/panel.usda".into(),
                prim: "/Panel".into(),
            },
        );
        let project = IdtxProject::create(&project_dir, manifest).unwrap();
        let prepared =
            prepare_runtime_prototypes(&project, None, &temporary.path().join("prepared")).unwrap();
        let payload = temporary.path().join("payload");
        let registry = package_runtime_prototypes(&prepared, &payload).unwrap();
        let relocated = temporary.path().join("relocated");
        std::fs::rename(&payload, &relocated).unwrap();
        let template = &registry["panel"];
        assert!(template.asset.is_relative());
        validate_prepared_template(&relocated.join(&template.asset), &template.prim).unwrap();
    }
}
