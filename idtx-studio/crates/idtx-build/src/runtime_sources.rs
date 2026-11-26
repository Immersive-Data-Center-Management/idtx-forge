//! Cook-only source policy over the complete owned staging tree, before stamping.
use crate::CookError;
use std::path::Path;

pub(crate) fn sanitize_tree(root: &Path) -> Result<(), CookError> {
    fn io(path: &Path, source: std::io::Error) -> CookError {
        CookError::Io {
            path: path.to_owned(),
            source,
        }
    }
    let entries = std::fs::read_dir(root)
        .map_err(|e| io(root, e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| io(root, e))?;
    for entry in entries {
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| io(&path, e))?;
        if kind.is_dir() {
            sanitize_tree(&path)?;
        } else if kind.is_file() {
            if idtx_usd::is_behavior_source_asset(&path.to_string_lossy()) {
                std::fs::remove_file(&path).map_err(|e| io(&path, e))?;
            } else if path.extension().and_then(|s| s.to_str()).is_some_and(|s| {
                matches!(
                    s.to_ascii_lowercase().as_str(),
                    "usd" | "usda" | "usdc" | "usdz"
                )
            }) {
                let scratch = tempfile::tempdir_in(root).map_err(|e| io(root, e))?;
                let filtered = scratch.path().join(path.file_name().expect("staged file"));
                if idtx_usd::copy_runtime_asset(&path, &filtered)? {
                    // Only this fresh, unpublished cook staging tree is touched.
                    // Its outer publication transaction protects the last payload.
                    std::fs::copy(&filtered, &path).map_err(|e| io(&path, e))?;
                }
                if path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("usdz"))
                {
                    crate::usdz_metadata::normalize(&path)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // Stored, 64-byte-aligned USDZ fixture, including deliberately unreachable
    // members. Dependency localization cannot construct that negative case.
    fn archive(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        fn word(out: &mut Vec<u8>, value: u16) {
            out.extend(value.to_le_bytes());
        }
        fn dword(out: &mut Vec<u8>, value: u32) {
            out.extend(value.to_le_bytes());
        }
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in entries {
            let mut crc = !0u32;
            for &byte in data {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
                }
            }
            crc = !crc;
            let offset = out.len() as u32;
            let mut pad = (64 - (out.len() + 30 + name.len()) % 64) % 64;
            if pad > 0 && pad < 4 {
                pad += 64;
            }
            dword(&mut out, 0x0403_4b50);
            for v in [20, 0, 0, 0, 33] {
                word(&mut out, v);
            }
            for v in [crc, data.len() as u32, data.len() as u32] {
                dword(&mut out, v);
            }
            word(&mut out, name.len() as u16);
            word(&mut out, pad as u16);
            out.extend(name.as_bytes());
            if pad > 0 {
                word(&mut out, 0x1986);
                word(&mut out, (pad - 4) as u16);
                out.resize(out.len() + pad - 4, 0);
            }
            out.extend(data);
            dword(&mut central, 0x0201_4b50);
            for v in [20, 20, 0, 0, 0, 33] {
                word(&mut central, v);
            }
            for v in [crc, data.len() as u32, data.len() as u32] {
                dword(&mut central, v);
            }
            for v in [name.len() as u16, 0, 0, 0, 0] {
                word(&mut central, v);
            }
            dword(&mut central, 0);
            dword(&mut central, offset);
            central.extend(name.as_bytes());
        }
        let start = out.len() as u32;
        out.extend(&central);
        dword(&mut out, 0x0605_4b50);
        for v in [0, 0, entries.len() as u16, entries.len() as u16] {
            word(&mut out, v);
        }
        dword(&mut out, central.len() as u32);
        dword(&mut out, start);
        word(&mut out, 0);
        out
    }

    fn members(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut at = 0;
        while bytes.get(at..at + 4) == Some(b"PK\x03\x04") {
            let len = u32::from_le_bytes(bytes[at + 18..at + 22].try_into().unwrap()) as usize;
            let name = u16::from_le_bytes(bytes[at + 26..at + 28].try_into().unwrap()) as usize;
            let extra = u16::from_le_bytes(bytes[at + 28..at + 30].try_into().unwrap()) as usize;
            let data = at + 30 + name + extra;
            assert_eq!(data % 64, 0);
            out.push((
                String::from_utf8(bytes[at + 30..at + 30 + name].to_vec()).unwrap(),
                bytes[data..data + len].to_vec(),
            ));
            at = data + len;
        }
        out
    }

    fn assert_runtime_archive(bytes: &[u8]) {
        for (name, bytes) in members(bytes) {
            assert!(
                !name.to_ascii_lowercase().ends_with(".rs"),
                "source member {name}"
            );
            if name.ends_with(".usdz") {
                assert_runtime_archive(&bytes);
            }
            if name.ends_with(".usda") {
                assert!(
                    !String::from_utf8_lossy(&bytes).contains("idtx:behavior:source"),
                    "source binding in {name}"
                );
            }
        }
    }

    #[test]
    fn complete_inventory_strips_unreachable_nested_sources_and_preserves_live_layers() {
        let dir = tempfile::tempdir().unwrap();
        let layer = br#"#usda 1.0
(defaultPrim = "Actor")
def Xform "Actor" (prepend variantSets = "mode") {
    asset idtx:behavior:module = @module.wasm@
    variantSet "mode" = {
        "dormant" {
            asset idtx:behavior:source = @leaked.rs@
        }
    }
}
"#
        .to_vec();
        let module = wat::parse_str("(module)").unwrap();
        let inner = archive(&[
            ("root.usda", layer.clone()),
            ("leaked.rs", b"PRIVATE_SOURCE".to_vec()),
            ("module.wasm", module.clone()),
        ]);
        let original = archive(&[
            ("root.usda", layer.clone()),
            ("unreachable.rs", b"PRIVATE_SOURCE".to_vec()),
            ("module.wasm", module.clone()),
            ("nested.usdz", inner),
            ("extra.usda", layer.clone()),
            ("image.bin", vec![1, 2, 3]),
        ]);
        let src = dir.path().join("original.usdz");
        std::fs::write(&src, &original).unwrap();
        let live = idtx_usd::StageService::open(&src).unwrap();
        let before = live.root_layer_text().unwrap();
        let out = dir.path().join("cooked.usdz");
        assert!(idtx_usd::copy_runtime_asset(&src, &out).unwrap());
        let cooked = std::fs::read(&out).unwrap();
        assert!(
            !cooked
                .windows(b"PRIVATE_SOURCE".len())
                .any(|bytes| bytes == b"PRIVATE_SOURCE")
        );
        assert_runtime_archive(&cooked);
        let entries = members(&cooked);
        assert_eq!(
            entries
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            [
                "root.usda",
                "module.wasm",
                "nested.usdz",
                "extra.usda",
                "image.bin"
            ]
        );
        assert_eq!(entries[1].1, module);
        assert_eq!(entries[4].1, [1, 2, 3]);
        assert_eq!(live.root_layer_text().unwrap(), before);
        assert_eq!(std::fs::read(&src).unwrap(), original);
        let repeated = dir.path().join("repeated.usdz");
        idtx_usd::copy_runtime_asset(&src, &repeated).unwrap();
        assert_eq!(
            std::fs::read(repeated).unwrap(),
            cooked,
            "nested archive metadata must be deterministic"
        );
        let loose = dir.path().join("loose.usda");
        std::fs::write(&loose, layer).unwrap();
        let mut loose_live = idtx_usd::StageService::open(&loose).unwrap();
        let edited = loose_live.root_layer_text().unwrap().replace(
            "asset idtx:behavior:module",
            "custom string note = \"unsaved\"\n    asset idtx:behavior:module",
        );
        loose_live.replace_root_layer_text(&edited).unwrap();
        let edited = loose_live.root_layer_text().unwrap();
        idtx_usd::copy_runtime_asset(&loose, &dir.path().join("loose-runtime.usda")).unwrap();
        assert_eq!(loose_live.root_layer_text().unwrap(), edited);
        assert!(loose_live.root_layer_is_dirty().unwrap());
    }

    fn inventory(root: &Path) -> Vec<PathBuf> {
        let mut paths = vec![];
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                paths.extend(inventory(&path));
            } else {
                paths.push(path);
            }
        }
        paths
    }

    #[test]
    fn every_cook_route_filters_declared_roots_and_builtins_before_hashing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/unused.rs"), "PRIVATE_SOURCE").unwrap();
        std::fs::write(
            root.join("assets/unused.usda"),
            "#usda 1.0\ndef Xform \"Unused\" {\nasset idtx:behavior:source = @unused.rs@\n}\n",
        )
        .unwrap();
        std::fs::write(root.join("level.usda"), "#usda 1.0\n(defaultPrim = \"World\")\ndef Xform \"World\" {\nasset idtx:behavior:source = @missing.rs@\n}\n").unwrap();
        let mut manifest = idtx_project::ProjectManifest::new("runtime sources");
        manifest.default_stage = Some("level.usda".into());
        let project = idtx_project::IdtxProject::create(&root, manifest).unwrap();
        for (i, target, loose) in [
            (0, crate::CookTarget::desktop("host"), false),
            (1, crate::CookTarget::desktop("host"), true),
            (2, crate::CookTarget::quest("aarch64-linux-android"), false),
            (
                3,
                crate::CookTarget::vision_pro("aarch64-apple-visionos"),
                false,
            ),
        ] {
            let mut plan =
                crate::CookPlan::from_project(&project, target, dir.path().join(format!("out{i}")))
                    .unwrap();
            plan.player = None;
            plan.xr.controllers.clear();
            plan.loose_levels = loose;
            let first = crate::cook(&plan).unwrap();
            for file in inventory(&first.out_dir) {
                assert!(
                    !file
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("rs")),
                    "{}",
                    file.display()
                );
                if file.extension().is_some_and(|e| e == "usda") {
                    assert!(
                        !std::fs::read_to_string(&file)
                            .unwrap()
                            .contains("@unused.rs@")
                    );
                }
            }
            assert_eq!(first.content_hash, crate::cook(&plan).unwrap().content_hash);
        }
        assert!(root.join("assets/unused.rs").is_file());
        assert!(
            std::fs::read_to_string(root.join("level.usda"))
                .unwrap()
                .contains("@missing.rs@")
        );
    }

    #[test]
    fn source_authored_prototype_cooks_keep_identical_names_and_hashes_after_relocation() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original");
        std::fs::create_dir_all(original.join("assets/behaviors/spin")).unwrap();
        std::fs::write(original.join("assets/behaviors/spin/behavior.rs"),
            "use idtx_behavior_sdk::prelude::*;\n#[derive(Default)] pub struct Spin;\n#[behavior(kind=\"runtime\")] impl Spin { fn on_update(&mut self, _: &Ctx, _: f32) -> Result<()> { Ok(()) } }\n").unwrap();
        std::fs::write(
            original.join("assets/prototype.usda"),
            r#"#usda 1.0
(defaultPrim = "Actor")
def Xform "Actor" (prepend apiSchemas = ["IdtxBehaviorAPI"]) {
    asset idtx:behavior:source = @behaviors/spin/behavior.rs@
    string idtx:behavior:uuid = "portable-source"
}
"#,
        )
        .unwrap();
        std::fs::write(
            original.join("level.usda"),
            "#usda 1.0\n(defaultPrim = \"World\")\ndef Xform \"World\" {}\n",
        )
        .unwrap();
        let mut manifest = idtx_project::ProjectManifest::new("Portable source");
        manifest.default_stage = Some("level.usda".into());
        manifest.runtime_prototypes.insert(
            "actor".into(),
            idtx_project::RuntimePrototypeConfig {
                asset: "assets/prototype.usda".into(),
                prim: "/Actor".into(),
            },
        );
        idtx_project::IdtxProject::create(&original, manifest).unwrap();
        let build = |root: &Path, out: &Path| {
            let project = idtx_project::IdtxProject::load(root.join("project.toml")).unwrap();
            let mut plan =
                crate::CookPlan::from_project(&project, crate::CookTarget::desktop("host"), out)
                    .unwrap();
            plan.player = None;
            plan.xr.controllers.clear();
            plan.behavior_sdk = Some(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .unwrap()
                    .join("idtx-behavior-sdk"),
            );
            crate::cook(&plan).unwrap()
        };
        let first = build(&original, &dir.path().join("first"));
        let relocated = dir.path().join("relocated");
        std::fs::rename(original, &relocated).unwrap();
        let second = build(&relocated, &dir.path().join("second"));
        assert_eq!(first.content_hash, second.content_hash);
        let first = std::fs::read(first.out_dir.join("runtime_prototypes/actor.usdz")).unwrap();
        let second = std::fs::read(second.out_dir.join("runtime_prototypes/actor.usdz")).unwrap();
        assert_eq!(first, second);
        assert!(
            members(&first)
                .iter()
                .any(|(name, _)| name.ends_with(".wasm")),
            "uninstantiated source behavior must be compiled and packaged"
        );
        assert!(
            relocated
                .join("assets/behaviors/spin/behavior.rs")
                .is_file()
        );
    }
}
