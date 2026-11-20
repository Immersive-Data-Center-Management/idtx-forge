use idtx_usd::{PackagingPurpose, StageError, StageService};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);
const SOURCE: &[u8] = b"// IDTX_AUTHORING_SOURCE_MARKER\nfn main() {}\n";
const MODULE: &[u8] = b"IDTX_COMPILED_MODULE_MARKER";
const REMOTE: &str = "https://assets.example.invalid/remote.usda";

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "idtx-packaging-purpose-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone, Copy)]
enum Packager {
    Local,
    ImportAnyway,
    KeepingRemote,
}

impl Packager {
    fn package(
        self,
        source: &Path,
        destination: &Path,
        purpose: PackagingPurpose,
    ) -> Result<idtx_usd::PackageResult, StageError> {
        match self {
            Self::Local => idtx_usd::package_stage(source, destination, purpose),
            Self::ImportAnyway => {
                idtx_usd::package_stage_dropping_unresolved(source, destination, purpose)
            }
            Self::KeepingRemote => {
                idtx_usd::package_stage_localize_keeping_remote(source, destination, purpose)
            }
        }
    }
}

const PACKAGERS: [Packager; 3] = [
    Packager::Local,
    Packager::ImportAnyway,
    Packager::KeepingRemote,
];

fn write_stage(dir: &Path, packager: Packager, with_module: bool) -> (PathBuf, PathBuf) {
    std::fs::create_dir_all(dir.join("behaviors")).unwrap();
    std::fs::create_dir_all(dir.join("layers")).unwrap();
    std::fs::write(dir.join("behaviors/cam.rs"), SOURCE).unwrap();
    let module = if with_module {
        std::fs::write(dir.join("behaviors/cam.wasm"), MODULE).unwrap();
        "asset idtx:behavior:module = @../behaviors/cam.wasm@"
    } else {
        ""
    };
    let extra = match packager {
        Packager::Local => String::new(),
        Packager::ImportAnyway => "asset missing = @./missing.png@".to_owned(),
        Packager::KeepingRemote => format!("asset remote = @{REMOTE}@"),
    };
    let layer = dir.join("layers/behavior.usda");
    std::fs::write(
        &layer,
        format!(
            "#usda 1.0\ndef Xform \"root\" {{\n    def Xform \"Behaved\" {{\n\
             asset idtx:behavior:source = @../behaviors/cam.rs@\n{module}\n{extra}\n}}\n}}\n"
        ),
    )
    .unwrap();
    let entry = dir.join("scene.usda");
    // Exercises dependency canonicalization and temporary defaultPrim adoption.
    std::fs::write(
        &entry,
        "#usda 1.0\n( subLayers = [@./layers/behavior.usda@] )\ndef Xform \"root\" {}\n",
    )
    .unwrap();
    (entry, layer)
}

fn assert_behavior(package: &Path, purpose: PackagingPurpose, with_module: bool) {
    let stage = StageService::open(package).expect("reopen USDZ");
    let prim = stage.prim_at_path("/root/Behaved").unwrap();
    let source = stage
        .resolve_prim_asset(prim, "idtx:behavior:source")
        .unwrap();
    if purpose == PackagingPurpose::PreserveAuthoring {
        let resolved = source.expect("import must retain a resolvable source binding");
        assert!(
            resolved.starts_with(&package.to_string_lossy().to_string()),
            "source must resolve INSIDE the import, not to the original: {resolved}"
        );
        assert_eq!(stage.read_asset(&resolved).unwrap(), SOURCE);
    } else {
        assert_eq!(source, None, "runtime source binding must be blank");
        let bytes = std::fs::read(package).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("IDTX_AUTHORING_SOURCE_MARKER"));
        assert!(
            !text.contains("cam.rs"),
            "runtime must not contain a source member"
        );
    }
    let module = stage
        .resolve_prim_asset(prim, "idtx:behavior:module")
        .unwrap();
    if with_module {
        let resolved = module.expect("compiled module must survive either purpose");
        assert!(resolved.starts_with(&package.to_string_lossy().to_string()));
        assert_eq!(stage.read_asset(&resolved).unwrap(), MODULE);
    } else {
        assert_eq!(module, None);
    }
}

#[test]
fn packaging_purpose_preserves_import_sources_and_strips_runtime_sources() {
    for packager in PACKAGERS {
        for with_module in [false, true] {
            let dir = TestDir::new();
            let (entry, layer) = write_stage(&dir.0, packager, with_module);
            let originals = [entry.clone(), layer, dir.0.join("behaviors/cam.rs")];
            let before: Vec<_> = originals
                .iter()
                .map(|p| std::fs::read(p).unwrap())
                .collect();
            let live = StageService::open(&entry).unwrap();
            for purpose in [
                PackagingPurpose::PreserveAuthoring,
                PackagingPurpose::Runtime,
            ] {
                let dst = dir.0.join(format!("{purpose:?}.usdz"));
                let result = packager.package(&entry, &dst, purpose).unwrap();
                assert_eq!(result.destination, dst, "{packager:?}, {purpose:?}");
                if matches!(packager, Packager::ImportAnyway) {
                    assert!(result.unresolved.iter().any(|p| p.contains("missing.png")));
                    assert!(!result.unresolved.iter().any(|p| p.contains("cam.rs")));
                }
                if matches!(packager, Packager::KeepingRemote) {
                    assert!(result.external.iter().any(|p| p == REMOTE));
                    assert!(
                        String::from_utf8_lossy(&std::fs::read(&dst).unwrap()).contains(REMOTE)
                    );
                }
                assert_behavior(&dst, purpose, with_module);
                let prim = live.prim_at_path("/root/Behaved").unwrap();
                let source = live
                    .resolve_prim_asset(prim, "idtx:behavior:source")
                    .unwrap()
                    .unwrap();
                assert_eq!(live.read_asset(&source).unwrap(), SOURCE);
                #[cfg(feature = "authoring")]
                assert!(!live.root_layer_is_dirty().unwrap());
                for (path, original) in originals.iter().zip(&before) {
                    assert_eq!(&std::fs::read(path).unwrap(), original);
                }
            }
        }
    }
}

#[test]
fn authoring_packaging_reports_missing_source_instead_of_silently_stripping_it() {
    let dir = TestDir::new();
    let (entry, layer) = write_stage(&dir.0, Packager::Local, true);
    std::fs::remove_file(dir.0.join("behaviors/cam.rs")).unwrap();
    let before = std::fs::read(&layer).unwrap();
    let dst = dir.0.join("missing-source.usdz");
    let result =
        idtx_usd::package_stage(&entry, &dst, PackagingPurpose::PreserveAuthoring).unwrap();
    assert!(result.unresolved.iter().any(|p| p.contains("cam.rs")));
    assert!(result.destination.as_os_str().is_empty());
    assert!(!dst.exists());
    let result = idtx_usd::package_stage_dropping_unresolved(
        &entry,
        &dst,
        PackagingPurpose::PreserveAuthoring,
    )
    .unwrap();
    assert!(result.unresolved.iter().any(|p| p.contains("cam.rs")));
    assert_eq!(std::fs::read(layer).unwrap(), before);
    assert!(dst.is_file());
}

#[cfg(feature = "authoring")]
#[test]
fn packaging_purpose_restores_live_layers_after_success_and_failure() {
    for packager in PACKAGERS {
        for purpose in [
            PackagingPurpose::PreserveAuthoring,
            PackagingPurpose::Runtime,
        ] {
            for dirty in [false, true] {
                let dir = TestDir::new();
                let (entry, layer) = write_stage(&dir.0, packager, true);
                let root_disk = std::fs::read(&entry).unwrap();
                let layer_disk = std::fs::read(&layer).unwrap();
                let mut live = StageService::open(&entry).unwrap();
                let mut dependency = StageService::open(&layer).unwrap();
                // Dirty both shared layers. PreserveAuthoring exercises the no-op
                // dependency rewrite; Runtime exercises an actual source clear.
                if dirty {
                    let root_text = live.root_layer_text().unwrap().replace(
                        "def Xform \"root\"",
                        "def Xform \"root\" ( documentation = \"unsaved root\" )",
                    );
                    live.replace_root_layer_text(&root_text).unwrap();
                    let layer_text = dependency.root_layer_text().unwrap().replace(
                        "def Xform \"Behaved\"",
                        "def Xform \"Behaved\" ( documentation = \"unsaved behavior\" )",
                    );
                    dependency.replace_root_layer_text(&layer_text).unwrap();
                }
                let root_before = live.root_layer_text().unwrap();
                let layer_before = dependency.root_layer_text().unwrap();

                // An existing non-empty directory cannot be replaced by the zip
                // writer. This forces failure AFTER the temporary rewrites.
                let blocked = dir.0.join("blocked.usdz");
                std::fs::create_dir(&blocked).unwrap();
                std::fs::write(blocked.join("sentinel"), b"untouched").unwrap();
                for failure in [false, true, false] {
                    let destination = if failure {
                        blocked.clone()
                    } else {
                        dir.0.join(format!(
                            "package-{}.usdz",
                            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
                        ))
                    };
                    // Also exercise the worker-thread/lifecycle-lock path used by
                    // desktop import. A subsequent success proves failure unlocked.
                    let source = entry.clone();
                    let result = std::thread::spawn(move || {
                        packager.package(&source, &destination, purpose)
                    })
                    .join()
                    .unwrap();
                    assert_eq!(result.is_err(), failure, "{packager:?}, {purpose:?}");
                    assert_eq!(live.root_layer_text().unwrap(), root_before);
                    assert_eq!(dependency.root_layer_text().unwrap(), layer_before);
                    assert_eq!(live.root_layer_is_dirty().unwrap(), dirty);
                    assert_eq!(dependency.root_layer_is_dirty().unwrap(), dirty);
                    assert_eq!(std::fs::read(&entry).unwrap(), root_disk);
                    assert_eq!(std::fs::read(&layer).unwrap(), layer_disk);
                    assert_eq!(
                        std::fs::read(blocked.join("sentinel")).unwrap(),
                        b"untouched"
                    );
                    assert!(!dir.0.join("blocked.usdz.localize-tmp").exists());
                }
            }
        }
    }
}
