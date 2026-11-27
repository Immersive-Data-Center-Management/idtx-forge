use super::*;
use std::sync::{Arc, Barrier};
use std::sync::atomic::{Ordering, AtomicU64};

pub static NEXT_TEST_FILE: AtomicU64 = AtomicU64::new(1);

#[path = "../../../tests/support/mod.rs"]
pub(crate) mod support;
use support::usd_fixture as fixture;

#[test]
fn stage_ownership_can_move_between_threads() {
    let fixture = fixture("scene/simple_hierarchy.usda");
    let stage = StageService::open(fixture).expect("open stage on caller");
    let generation = stage.generation();

    let mut stage = std::thread::spawn(move || {
        let mut stage = stage;
        let projection = stage.project_viewport().expect("project on worker");
        assert_eq!(projection.generation, generation);
        stage
    })
    .join()
    .expect("stage worker");

    assert_eq!(stage.generation(), generation);
    assert!(!stage.topology().is_empty());
    stage.project_topology().expect("reuse stage on caller");
}


/// Shadow-mode parsing: `StageMetadata.shadow_mode` resolves the root-layer
/// `customLayerData["idtx:shadowMode"]` token (`off`/`hard`/`smooth`/`soft`)
/// with a fallback to the legacy `idtx:softShadows` bool (`true`→soft,
/// `false`→hard), and defaults to `Off` when neither key is authored. Requires
/// the USD lib to link (`IDTX_USD_DIR`), hence `#[ignore]`.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn shadow_mode_parses_from_custom_layer_data() {
    let shadow_fixture = |name: &str| -> PathBuf { fixture(&format!("shadows/{name}")) };
    let mode = |name: &str| -> ShadowMode {
        StageService::open(shadow_fixture(name))
            .unwrap_or_else(|e| panic!("open {name}: {e:?}"))
            .metadata()
            .shadow_mode
    };

    // Explicit four-state token.
    assert_eq!(
        mode("shadow_mode_soft.usda"),
        ShadowMode::Soft,
        "token soft → Soft"
    );
    assert_eq!(
        mode("shadow_mode_smooth.usda"),
        ShadowMode::Smooth,
        "token smooth → Smooth"
    );
    assert_eq!(
        mode("shadow_mode_hard.usda"),
        ShadowMode::Hard,
        "token hard → Hard"
    );
    assert_eq!(
        mode("shadow_flag_off.usda"),
        ShadowMode::Off,
        "token off → Off"
    );

    // Legacy bool fallback: true→soft, false→hard.
    assert_eq!(
        mode("shadow_single_distant.usda"),
        ShadowMode::Soft,
        "legacy softShadows=true → Soft"
    );
    assert_eq!(
        mode("shadow_legacy_false_hard.usda"),
        ShadowMode::Hard,
        "legacy softShadows=false → Hard"
    );

    // Neither key → opt-in default Off.
    assert_eq!(mode("shadow_no_key.usda"), ShadowMode::Off, "no key → Off");

    // Token wins over the legacy bool; unknown token → Off.
    assert_eq!(
        mode("shadow_mode_token_wins.usda"),
        ShadowMode::Off,
        "token off must win over legacy softShadows=true"
    );
    assert_eq!(
        mode("shadow_mode_unknown.usda"),
        ShadowMode::Off,
        "unknown token → Off"
    );
}

/// Apply and remove a single-apply API schema (`UsdLuxShadowAPI`) on a
/// DistantLight through the `apply-api`/`remove-api` structural kinds, the
/// backend of the light inspector's "Add Shadow Casting" button. Requires
/// the USD lib + authoring feature.

#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn apply_and_remove_shadow_api_on_distant_light() {
    let fixture = fixture("lighting/lights_all_types.usda");
    let mut svc = StageService::open(&fixture).expect("open lights fixture");
    svc.project_topology().expect("topology");

    let sun = svc.prim_at_path("/World/Sun").expect("distant light prim");
    assert!(
        !svc.prim_has_api(sun, "ShadowAPI").expect("prim_has_api"),
        "fixture must start without ShadowAPI"
    );

    // Apply → HasAPI + apiSchemas summary contains ShadowAPI.
    svc.apply_structural_command(&StructuralCommand::ApplyApi {
        path: "/World/Sun".to_owned(),
        schema: "ShadowAPI".to_owned(),
    })
    .expect("apply ShadowAPI");
    assert!(
        svc.prim_has_api(sun, "ShadowAPI")
            .expect("prim_has_api after apply"),
        "ShadowAPI must be applied"
    );
    let props = svc.inspect_prim(sun).expect("inspect after apply");
    let api_summary = props
        .iter()
        .find(|p| p.name == "apiSchemas")
        .map(|p| p.value_summary.clone())
        .unwrap_or_default();
    assert!(
        api_summary.contains("ShadowAPI"),
        "apiSchemas summary must list ShadowAPI; got {api_summary:?}"
    );

    // Remove → symmetric reversal.
    svc.apply_structural_command(&StructuralCommand::RemoveApi {
        path: "/World/Sun".to_owned(),
        schema: "ShadowAPI".to_owned(),
    })
    .expect("remove ShadowAPI");
    assert!(
        !svc.prim_has_api(sun, "ShadowAPI")
            .expect("prim_has_api after remove"),
        "ShadowAPI must be removed"
    );

    // An unknown schema name is rejected (does not silently succeed).
    assert!(
        svc.apply_structural_command(&StructuralCommand::ApplyApi {
            path: "/World/Sun".to_owned(),
            schema: "NotARealAPI".to_owned(),
        })
        .is_err(),
        "unknown API schema must be rejected"
    );
}

/// A loose scene whose material references textures that cannot resolve on
/// this machine (a UDIM placeholder with no tiles, plus a foreign-OS
/// absolute path) must still SAVE in place. Such pre-existing unresolved
/// dependencies are orthogonal to rewriting the root layer — all a loose
/// save touches — yet the previous `Save`-mode verifier aborted with
/// "source stage has unresolved dependencies and cannot be verified for
/// save". Regression guard for the MarketScene/FullScene in-place-save
/// failure. Requires the USD lib.

#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn loose_save_tolerates_preexisting_unresolved_dependencies() {
    let source = fixture("authoring/loose_save_unresolved_deps.usda");
    // Copy the fixture into a private work directory and save it back to the
    // SAME path — the exact "Save in place" gesture that failed on FullScene.
    // Relative references (`./textures/<UDIM>...`) anchor to the same
    // directory on both the source and the atomically-rewritten layer, so the
    // (unresolved) dependency closure is unchanged; the pre-existing unresolved
    // texture paths must not abort the save.
    let workdir = std::env::temp_dir().join(format!(
        "idtx_loose_save_unresolved_{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&workdir).expect("create work directory");
    let scene = workdir.join("scene.usda");
    std::fs::copy(&source, &scene).expect("copy fixture into work directory");

    let svc = StageService::open(&scene).expect("open scene with unresolved textures");
    svc.save_stage(&scene)
        .expect("in-place loose save must tolerate pre-existing unresolved deps");

    // And it must round-trip: reopen and confirm the authored content survived.
    let reopened = StageService::open(&scene).expect("reopen saved scene");
    assert!(reopened.prim_at_path("/World/Ground").is_some());
    let _ = std::fs::remove_dir_all(&workdir);
}

#[test]
#[ignore = "ground-truth probe against an external BarScene asset"]
fn barscene_light_ground_truth() {
    let path = "/Users/D054981/Downloads/BarScene/Library/Assets/barScene/barScene.usd";
    let mut stage = StageService::open(path).expect("open barScene.usd");

    let meta = stage.metadata();
    println!("=== STAGE METADATA ===");
    println!("root_identifier = {}", meta.root_identifier);
    println!("default_prim    = {:?}", meta.default_prim);
    println!("up_axis         = {}", meta.up_axis);
    println!("meters_per_unit = {}", meta.meters_per_unit);

    // Enumerate every prim whose type name ends in "Light" from topology.
    stage.project_topology().expect("project topology");
    let light_prims: Vec<(PrimId, String, String)> = stage
        .topology()
        .iter()
        .filter(|r| r.type_name.ends_with("Light"))
        .map(|r| {
            let p = stage
                .path_for_prim(r.prim)
                .unwrap_or("<unknown>")
                .to_string();
            (r.prim, r.type_name.clone(), p)
        })
        .collect();

    println!();
    println!("=== TOPOLOGY LIGHT PRIMS (type_name ends with \"Light\") ===");
    println!("count = {}", light_prims.len());
    for (_, type_name, p) in &light_prims {
        println!("  {p}  [{type_name}]");
    }

    let projection = stage.project_viewport().expect("project viewport");

    println!();
    println!("=== PROJECTED LIGHTS ===");
    println!("count = {}", projection.lights.len());
    for light in &projection.lights {
        let p = stage
            .path_for_prim(light.prim)
            .unwrap_or("<unknown>")
            .to_string();
        let (kind_name, radius, width, height) = match light.kind {
            LightKind::Distant { angle } => (format!("Distant(angle={angle})"), None, None, None),
            LightKind::Sphere { radius } => ("Sphere".to_string(), Some(radius), None, None),
            LightKind::Rect { width, height } => {
                ("Rect".to_string(), None, Some(width), Some(height))
            }
            LightKind::Disk { radius } => ("Disk".to_string(), Some(radius), None, None),
            LightKind::Cylinder { radius, length } => (
                format!("Cylinder(length={length})"),
                Some(radius),
                None,
                None,
            ),
            LightKind::Dome => ("Dome".to_string(), None, None, None),
            LightKind::DomeProcedural => ("DomeProcedural".to_string(), None, None, None),
        };
        println!("--- {p} ---");
        println!("  kind        = {kind_name}");
        println!("  intensity   = {}", light.intensity);
        println!("  exposure    = {}", light.exposure);
        println!("  normalize   = {}", light.normalize);
        println!("  radius      = {radius:?}");
        println!("  width       = {width:?}");
        println!("  height      = {height:?}");
        println!("  color       = {:?}", light.color);
        println!("  diffuse     = {}", light.diffuse);
        println!("  specular    = {}", light.specular);
        println!("  shadow_en   = {}", light.shadow_enabled);
        println!("  temperature = {:?}", light.temperature);
        println!(
            "  world_pos   = [{}, {}, {}]",
            light.world_matrix[12], light.world_matrix[13], light.world_matrix[14]
        );
    }

    println!();
    println!("=== PROJECTION DIAGNOSTICS ===");
    for d in &projection.diagnostics {
        println!("  {d}");
    }
}

#[test]
fn official_dependency_packaging_reopens_as_authoritative_usdz() {
    let destination = std::env::temp_dir().join(format!(
        "idtx-package-{}-{}.usdz",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = package_stage(
        fixture("scene/simple_hierarchy.usda"),
        &destination,
        PackagingPurpose::PreserveAuthoring,
    )
    .expect("package stage through OpenUSD");
    assert!(result.unresolved.is_empty());
    // A clean, fully-resolvable stage packages without diagnostics — the
    // Conductor "Import" flow relies on an empty `warnings` meaning
    // "close the modal and open immediately" (spec case 3, no-warnings path).
    assert!(
        result.warnings.is_empty(),
        "clean package emitted diagnostics: {:?}",
        result.warnings
    );
    assert!(
        result
            .dependencies
            .iter()
            .any(|dependency| { dependency.ends_with("simple_hierarchy.usda") })
    );
    let packaged = StageService::open(&destination).expect("official package reopen");
    assert!(packaged.prim_at_path("/World/Group/Cube").is_some());
    std::fs::remove_file(destination).expect("remove package fixture");
}

/// Packaging an asset that has **no** `defaultPrim` but exactly one root
/// `def` prim must stamp that prim as the package's `defaultPrim`, so the
/// result is usable as a `reference`/`payload` target. Without this, a
/// composition arc that names no prim path (`@asset.usdz@`) resolves to the
/// asset's (absent) defaultPrim and composes nothing — the "payload renders
/// nothing" trap for DCC exports (e.g. Houdini) that omit a defaultPrim.
///
/// Also proves the two invariants the fix must not break: the source file on
/// disk is left byte-for-byte untouched (the defaultPrim is adopted in memory
/// only), and composing the package as a payload actually pulls its content.
///
/// Requires the USD lib to link (`IDTX_USD_DIR`), hence `#[ignore]`.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn packaging_adopts_sole_root_as_default_prim() {
    let dir = std::env::temp_dir().join(format!(
        "idtx-defaultprim-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let source = dir.join("no_default_prim.usda");
    // No `defaultPrim` metadata; a single root `def` prim `Thing`.
    std::fs::write(
        &source,
        "#usda 1.0\n(\n    upAxis = \"Y\"\n    metersPerUnit = 1\n)\n\n\
             def Xform \"Thing\"\n{\n\
             \x20\x20def Cube \"Box\"\n\x20\x20{\n\x20\x20\x20\x20double size = 1\n\x20\x20}\n}\n",
    )
    .expect("write fixture");
    let before = std::fs::read(&source).expect("read fixture");

    let destination = dir.join("no_default_prim.usdz");
    let result = package_stage(&source, &destination, PackagingPurpose::PreserveAuthoring)
        .expect("package stage");
    assert!(result.unresolved.is_empty());
    assert!(destination.is_file(), "a package file must be written");

    // Source on disk is untouched (the defaultPrim is adopted in memory only).
    let after = std::fs::read(&source).expect("re-read fixture");
    assert_eq!(before, after, "source fixture must be left untouched");

    // The package now declares `/Thing` as its defaultPrim.
    let packaged = StageService::open(&destination).expect("reopen package");
    assert_eq!(
        packaged.metadata().default_prim.as_deref(),
        Some("/Thing"),
        "sole root prim must become the package defaultPrim"
    );

    // And it composes as a payload with no explicit prim-path target.
    let host = dir.join("host.usda");
    std::fs::write(
        &host,
        "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\n\
             def Xform \"World\"\n{\n\
             \x20\x20def Xform \"Instance\" (\n\
             \x20\x20\x20\x20prepend payload = @./no_default_prim.usdz@\n\x20\x20) {}\n}\n",
    )
    .expect("write host");
    let mut composed = StageService::open(&host).expect("open payload host");
    assert!(
        composed.prim_at_path("/World/Instance/Box").is_some(),
        "payload content must compose under the host prim"
    );
    let recs = composed.project_topology().expect("host topology");
    assert!(
        recs.iter()
            .any(|r| r.name == "Box" && r.type_name == "Cube"),
        "payloaded Cube must appear in the composed topology; got {} prims",
        recs.len()
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `package_stage_localize_keeping_remote` LOCALIZES local references —
/// including a **search-path** ref (`@assets/widget.usda@`, no `./`) that only
/// resolves via the asset search path — to package-internal paths so the usdz
/// composes on any machine, while PRESERVING a remote (`http(s)://`) reference
/// verbatim (reported in `external`). This is the regression test for the
/// on-device "no assets" bug: a search-path ref left un-localized fails to open
/// inside the package. No HTTP resolver is installed, so the remote URL is
/// unresolvable (packaging-mode condition); its authored bytes survive in the
/// archive. Requires the USD lib to link (`IDTX_USD_DIR`), hence `#[ignore]`.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn package_localize_keeps_remote_url_and_internalizes_search_path_ref() {
    const REMOTE_URL: &str = "https://assets.example.invalid/pack/remote.usda";

    let dir = std::env::temp_dir().join(format!(
        "idtx-keep-remote-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join("stages")).expect("create temp dir");
    std::fs::create_dir_all(dir.join("assets")).expect("create assets dir");

    // Local asset, referenced from the entry by a SEARCH-PATH ref that only
    // resolves against the asset search path (the root), NOT layer-relative.
    std::fs::write(
            dir.join("assets/widget.usda"),
            "#usda 1.0\n(\n    defaultPrim = \"Widget\"\n)\ndef Xform \"Widget\"\n{\n    def Mesh \"LocalMesh\" {}\n}\n",
        )
        .expect("write local asset");
    // A local file referenced by an ABSOLUTE asset-valued attribute — the shape
    // of a cooked `idtx:behavior:module = @/Users/…/x.wasm@`, which left
    // un-localized dangles on another machine. Localization must copy it in and
    // rewrite the path package-internal.
    std::fs::write(dir.join("assets/mod.bin"), b"IDTX_MODULE_BYTES_MARKER")
        .expect("write module asset");
    let module_abs = dir.join("assets/mod.bin");
    let entry = dir.join("stages/s.usda");
    std::fs::write(
            &entry,
            format!(
                "#usda 1.0\n(\n    defaultPrim = \"root\"\n)\n\
                 def Xform \"root\"\n{{\n\
                 \x20\x20def \"local\" (\n\x20\x20\x20\x20prepend references = @assets/widget.usda@</Widget>\n\x20\x20)\n\x20\x20{{\n\x20\x20\x20\x20asset idtx:behavior:module = @{}@\n\x20\x20}}\n\
                 \x20\x20def \"remote\" (\n\x20\x20\x20\x20prepend payload = @{REMOTE_URL}@\n\x20\x20) {{}}\n}}\n",
                module_abs.display()
            ),
        )
        .expect("write entry");

    // Anchor the search path at the project root so `@assets/widget.usda@`
    // resolves during localization (as the cook does for a project).
    set_default_asset_search_path(&[dir.clone()]).expect("set search path");

    let dst = dir.join("s.usdz");
    let result = package_stage_localize_keeping_remote(
        &entry,
        &dst,
        PackagingPurpose::PreserveAuthoring,
    )
    .expect("localize keeps remote");
    assert!(dst.is_file(), "package written");
    assert!(
        result.external.iter().any(|u| u.contains("remote.usda")),
        "remote URL reported external; got {:?}",
        result.external
    );

    // The remote URL survives verbatim (uncompressed usdz → literal bytes) and
    // was NOT remapped into an internal bucket.
    let text = String::from_utf8_lossy(&std::fs::read(&dst).unwrap()).into_owned();
    assert!(
        text.contains(REMOTE_URL),
        "authored remote URL must survive verbatim in the usdz"
    );
    // The absolute module asset was localized: its bytes are embedded and the
    // build-host absolute path no longer appears authored in the package.
    assert!(
        text.contains("IDTX_MODULE_BYTES_MARKER"),
        "absolute asset-valued attribute must be localized into the package"
    );
    assert!(
        !text.contains(&module_abs.display().to_string()),
        "absolute build-host asset path must be rewritten package-internal, not left authored"
    );
    assert!(
        !text.contains("0/remote.usda"),
        "remote ref must NOT be remapped into an internal bucket"
    );

    // The search-path local ref must now RESOLVE inside the package: open the
    // usdz and confirm the referenced mesh composes (the remote payload is
    // unresolvable with no server, which only warns). This is exactly what
    // failed on device when the ref was left un-localized.
    let mut staged = StageService::open(&dst).expect("reopen localized package");
    let recs = staged.project_topology().expect("topology");
    assert!(
        recs.iter()
            .any(|r| r.name == "LocalMesh" && r.type_name == "Mesh"),
        "search-path local ref must localize + compose inside the package; got {} prims",
        recs.len()
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Regression for the desktop-cook build break: the publish step canonicalizes
/// the output dir, yielding a Windows `\\?\` verbatim path. The localize-and-zip
/// packager composed child paths as `dir + "/" + rel`; under the Win32 `\\?\`
/// namespace `/` is NOT a separator, so every `SdfZipFileWriter::AddFile` failed
/// to open its file ("Failed to map … : No error") and the whole assembly threw.
/// The wrapper must strip the verbatim prefix. Windows-only (the prefix only
/// arises there); requires the USD lib to link (`IDTX_USD_DIR`).
#[test]
#[cfg(windows)]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn package_localize_tolerates_verbatim_prefixed_destination() {
    const REMOTE_URL: &str = "https://assets.example.invalid/pack/remote.usda";

    let dir = std::env::temp_dir().join(format!(
        "idtx-verbatim-dst-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join("assets")).expect("create dirs");
    std::fs::write(
        dir.join("assets/widget.usda"),
        "#usda 1.0\n(\n    defaultPrim = \"Widget\"\n)\ndef Xform \"Widget\" {}\n",
    )
    .expect("write asset");
    let entry = dir.join("s.usda");
    std::fs::write(
        &entry,
        format!(
            "#usda 1.0\n(\n    defaultPrim = \"root\"\n)\n\
             def Xform \"root\"\n{{\n\
             \x20\x20def \"local\" (\n\x20\x20\x20\x20prepend references = @assets/widget.usda@</Widget>\n\x20\x20) {{}}\n\
             \x20\x20def \"remote\" (\n\x20\x20\x20\x20prepend payload = @{REMOTE_URL}@\n\x20\x20) {{}}\n}}\n",
        ),
    )
    .expect("write entry");
    set_default_asset_search_path(&[dir.clone()]).expect("set search path");

    // Canonicalize to reproduce exactly the publish step's `\\?\` verbatim prefix.
    let canon = std::fs::canonicalize(&dir).expect("canonicalize");
    assert!(
        canon.to_string_lossy().starts_with(r"\\?\"),
        "precondition: canonicalize must yield a verbatim path; got {}",
        canon.display()
    );
    let dst = canon.join("s.usdz");

    package_stage_localize_keeping_remote(&entry, &dst, PackagingPurpose::Runtime)
        .expect("a verbatim-prefixed destination must package without error");
    assert!(
        dir.join("s.usdz").is_file(),
        "usdz must be written under the plain (non-verbatim) spelling"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A design-time behavior SOURCE (`idtx:behavior:source = @….rs@`) must never be
/// packaged into a cooked runtime artifact: the runtime ticks the compiled
/// `.wasm` module, not the Rust source. USD's localizer would otherwise collect
/// the `.rs` because `idtx:behavior:source` is an `asset`-typed attribute. The
/// remote-keeping packager strips it. Requires the USD lib (`IDTX_USD_DIR`).
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn package_localize_strips_behavior_source_rs() {
    const REMOTE_URL: &str = "https://assets.example.invalid/pack/remote.usda";
    const RS_MARKER: &str = "IDTX_RS_SOURCE_MARKER";
    const WASM_MARKER: &str = "IDTX_WASM_MODULE_MARKER";

    let dir = std::env::temp_dir().join(format!(
        "idtx-strip-rs-localize-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join("behaviors")).expect("create dirs");
    std::fs::write(dir.join("behaviors/cam.rs"), RS_MARKER).expect("write .rs");
    std::fs::write(dir.join("behaviors/cam.wasm"), WASM_MARKER).expect("write .wasm");
    let entry = dir.join("s.usda");
    std::fs::write(
        &entry,
        format!(
            "#usda 1.0\n(\n    defaultPrim = \"root\"\n)\n\
             def Xform \"root\"\n{{\n\
             \x20\x20def \"Behaved\"\n\x20\x20{{\n\
             \x20\x20\x20\x20asset idtx:behavior:source = @./behaviors/cam.rs@\n\
             \x20\x20\x20\x20asset idtx:behavior:module = @./behaviors/cam.wasm@\n\x20\x20}}\n\
             \x20\x20def \"remote\" (\n\x20\x20\x20\x20prepend payload = @{REMOTE_URL}@\n\x20\x20) {{}}\n}}\n",
        ),
    )
    .expect("write entry");
    set_default_asset_search_path(&[dir.clone()]).expect("set search path");

    let dst = dir.join("s.usdz");
    package_stage_localize_keeping_remote(&entry, &dst, PackagingPurpose::Runtime)
        .expect("localize keeps remote");
    assert!(dst.is_file(), "package written");

    let text = String::from_utf8_lossy(&std::fs::read(&dst).unwrap()).into_owned();
    assert!(
        !text.contains(RS_MARKER),
        "design-time .rs source must not be embedded in the package"
    );
    assert!(
        !text.contains("cam.rs"),
        "the .rs asset path must be blanked, not authored in the package"
    );
    assert!(
        text.contains(WASM_MARKER),
        "the compiled .wasm module must still be localized into the package"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The one-shot (non-remote) packager strips the same design-time `.rs` source as
/// the remote-keeping path, so a level without remote refs (which still gets the
/// player rig spliced in per-level mode) does not ship Rust source either.
/// Requires the USD lib (`IDTX_USD_DIR`).
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn package_stage_strips_behavior_source_rs() {
    const RS_MARKER: &str = "IDTX_RS_SOURCE_MARKER";
    const WASM_MARKER: &str = "IDTX_WASM_MODULE_MARKER";

    let dir = std::env::temp_dir().join(format!(
        "idtx-strip-rs-oneshot-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join("behaviors")).expect("create dirs");
    std::fs::write(dir.join("behaviors/cam.rs"), RS_MARKER).expect("write .rs");
    std::fs::write(dir.join("behaviors/cam.wasm"), WASM_MARKER).expect("write .wasm");
    let entry = dir.join("s.usda");
    std::fs::write(
        &entry,
        "#usda 1.0\n(\n    defaultPrim = \"root\"\n)\n\
         def Xform \"root\"\n{\n\
         \x20\x20def \"Behaved\"\n\x20\x20{\n\
         \x20\x20\x20\x20asset idtx:behavior:source = @./behaviors/cam.rs@\n\
         \x20\x20\x20\x20asset idtx:behavior:module = @./behaviors/cam.wasm@\n\x20\x20}\n}\n",
    )
    .expect("write entry");
    set_default_asset_search_path(&[dir.clone()]).expect("set search path");

    let dst = dir.join("s.usdz");
    package_stage(&entry, &dst, PackagingPurpose::Runtime).expect("package one-shot");
    assert!(dst.is_file(), "package written");

    let text = String::from_utf8_lossy(&std::fs::read(&dst).unwrap()).into_owned();
    assert!(
        !text.contains(RS_MARKER),
        "design-time .rs source must not be embedded in the one-shot package"
    );
    assert!(
        !text.contains("cam.rs"),
        "the .rs asset path must be blanked in the one-shot package"
    );
    assert!(
        text.contains(WASM_MARKER),
        "the compiled .wasm module must still be packaged"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Regression: a MISSING / misspelled design-time `idtx:behavior:source` `.rs`
/// must NOT block the one-shot packager. Because the attribute is asset-typed,
/// `UsdUtilsComputeAllDependencies` reports the absent `.rs` as *unresolved*;
/// before the fix `package_stage` refused outright (writing no file), and the
/// cook then failed far away reading a usdz that was never created
/// (`No such file or directory`). The `.rs` is design-time only — stripped from
/// the package regardless — so it is partitioned out of the unresolved gate: the
/// package is still written (with the `.rs` blanked and the `.wasm` kept), and
/// the dropped source is surfaced on `warnings`, not `unresolved`.
/// Requires the USD lib (`IDTX_USD_DIR`).
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn package_stage_tolerates_missing_behavior_source_rs() {
    const WASM_MARKER: &str = "IDTX_WASM_MODULE_MARKER";

    let dir = std::env::temp_dir().join(format!(
        "idtx-missing-rs-oneshot-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join("behaviors")).expect("create dirs");
    // Deliberately DO NOT write behaviors/cam.rs — it is referenced but absent.
    std::fs::write(dir.join("behaviors/cam.wasm"), WASM_MARKER).expect("write .wasm");
    let entry = dir.join("s.usda");
    std::fs::write(
        &entry,
        "#usda 1.0\n(\n    defaultPrim = \"root\"\n)\n\
         def Xform \"root\"\n{\n\
         \x20\x20def \"Behaved\"\n\x20\x20{\n\
         \x20\x20\x20\x20asset idtx:behavior:source = @./behaviors/cam.rs@\n\
         \x20\x20\x20\x20asset idtx:behavior:module = @./behaviors/cam.wasm@\n\x20\x20}\n}\n",
    )
    .expect("write entry");
    set_default_asset_search_path(&[dir.clone()]).expect("set search path");

    let dst = dir.join("s.usdz");
    let result = package_stage(&entry, &dst, PackagingPurpose::Runtime)
        .expect("package one-shot despite missing .rs");
    assert!(
        dst.is_file(),
        "package must be written even though the behavior-source .rs is missing"
    );
    assert!(
        !result.unresolved.iter().any(|u| u.contains("cam.rs")),
        "a missing design-time .rs must be partitioned out of the unresolved gate, not block the package: {:?}",
        result.unresolved
    );

    let text = String::from_utf8_lossy(&std::fs::read(&dst).unwrap()).into_owned();
    assert!(
        !text.contains("cam.rs"),
        "the missing .rs asset path must be blanked in the package"
    );
    assert!(
        text.contains(WASM_MARKER),
        "the compiled .wasm module must still be packaged"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn packaging_leaves_multi_root_default_prim_unset() {
    let dir = std::env::temp_dir().join(format!(
        "idtx-defaultprim-multi-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let source = dir.join("two_roots.usda");
    std::fs::write(
        &source,
        "#usda 1.0\n\ndef Xform \"A\" {}\ndef Xform \"B\" {}\n",
    )
    .expect("write fixture");

    let destination = dir.join("two_roots.usdz");
    package_stage(&source, &destination, PackagingPurpose::PreserveAuthoring)
        .expect("package stage");
    let packaged = StageService::open(&destination).expect("reopen package");
    assert_eq!(
        packaged.metadata().default_prim,
        None,
        "ambiguous multi-root layer must not have a defaultPrim guessed"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Packaging a stage that references a **missing** asset must never silently
/// succeed: the missing reference has to reach the caller so the Conductor
/// "Import" flow can tell the user which asset is broken. There are two
/// legitimate surfacing paths and this test accepts either:
///
/// * dependency analysis flags it in `unresolved` (so `package_stage`
///   short-circuits before attempting a doomed package — destination stays
///   empty), or
/// * it slips past analysis, `UsdUtilsCreateNewUsdzPackage` fails, and the
///   captured "Failed to resolve reference @…@" diagnostic is folded into the
///   returned error.
///
/// Both paths drive the Import modal to its "can't import" state; a silent
/// clean package would be the bug.
///
/// Requires the USD lib to link, hence `#[ignore]`. Self-contained: it
/// authors a throwaway `.usda` in the temp dir that references a texture
/// which does not exist, so no committed fixture is needed.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn packaging_missing_reference_captures_diagnostics() {
    let dir = std::env::temp_dir().join(format!(
        "idtx-import-warn-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let source = dir.join("broken.usda");
    // A material sampling a texture asset that is not on disk. Packaging
    // tries (and fails) to localize `@./missing_texture.png@`.
    std::fs::write(
        &source,
        "#usda 1.0\n\
             def Material \"Mat\"\n{\n\
             \x20\x20token outputs:surface.connect = </Mat/Preview.outputs:surface>\n\
             \x20\x20def Shader \"Preview\"\n\x20\x20{\n\
             \x20\x20\x20\x20uniform token info:id = \"UsdPreviewSurface\"\n\
             \x20\x20\x20\x20color3f inputs:diffuseColor.connect = </Mat/Tex.outputs:rgb>\n\
             \x20\x20\x20\x20token outputs:surface\n\x20\x20}\n\
             \x20\x20def Shader \"Tex\"\n\x20\x20{\n\
             \x20\x20\x20\x20uniform token info:id = \"UsdUVTexture\"\n\
             \x20\x20\x20\x20asset inputs:file = @./missing_texture.png@\n\
             \x20\x20\x20\x20float3 outputs:rgb\n\x20\x20}\n}\n",
    )
    .expect("write broken fixture");

    let destination = dir.join("broken.usdz");
    match package_stage(&source, &destination, PackagingPurpose::PreserveAuthoring) {
        Ok(result) => {
            // Dependency analysis flagged the missing asset, so packaging was
            // (correctly) never attempted.
            assert!(
                result
                    .unresolved
                    .iter()
                    .any(|unresolved| unresolved.contains("missing_texture")),
                "missing texture should be reported as unresolved, got: {:?}",
                result.unresolved
            );
            assert!(
                result.destination.as_os_str().is_empty(),
                "no package produced when a reference is unresolved"
            );
            assert!(!destination.is_file(), "no package file written on failure");
        }
        Err(err) => {
            // Packaging was attempted and failed; the captured diagnostic must
            // name the missing asset.
            let message = err.to_string();
            assert!(
                message.contains("missing_texture") || message.contains("Failed to resolve"),
                "packaging error should name the unresolved reference, got: {message}"
            );
        }
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// "Import anyway" path: a stage carrying an unresolved reference must still
/// package once the broken reference is dropped, AND the on-disk source must
/// be left byte-for-byte unchanged (`OpenUSD` localizes in-memory copies, so
/// no backup/restore is involved). Uses the committed fixture whose
/// `bar_scene` references a non-existent `@./does_not_exist.usda@`.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn dropping_unresolved_packages_and_leaves_source_untouched() {
    let source = fixture("authoring/save_with_unresolved_reference.usda");
    // Snapshot the source bytes up front to prove it is never modified.
    let before = std::fs::read(&source).expect("read fixture");

    // Baseline: plain packaging refuses — reports the unresolved reference
    // and writes no file (or, equivalently, errors out). Either is fine; the
    // point is that it does NOT silently succeed. (An `Err` here is also an
    // acceptable "refusal", so only the success branch is asserted.)
    let baseline_dest = std::env::temp_dir().join(format!(
        "idtx-strip-baseline-{}-{}.usdz",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    if let Ok(baseline) = package_stage(&source, &baseline_dest, PackagingPurpose::PreserveAuthoring) {
        assert!(
            !baseline.unresolved.is_empty(),
            "fixture should have an unresolved reference"
        );
        assert!(
            baseline.destination.as_os_str().is_empty() && !baseline_dest.is_file(),
            "plain packaging must not write a file for an unresolved stage"
        );
    }

    // Import anyway: clear the unresolved refs from in-memory layer copies,
    // then package.
    let dest = std::env::temp_dir().join(format!(
        "idtx-strip-{}-{}.usdz",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));

    let result = package_stage_dropping_unresolved(&source, &dest, PackagingPurpose::PreserveAuthoring)
        .expect("packaging with unresolved refs dropped should succeed");
    assert!(dest.is_file(), "a package file must be written");
    assert_eq!(
        result.destination, dest,
        "result should report the written destination"
    );
    assert!(
        !result.unresolved.is_empty(),
        "the dropped references should be reported back to the caller"
    );

    // The crux: the source file on disk is byte-for-byte identical.
    let after = std::fs::read(&source).expect("re-read fixture");
    assert_eq!(before, after, "source fixture must be left untouched");

    // Content must survive: the fixture's local `Body` mesh is still present
    // in the package (the broken reference is dropped, not the geometry).
    // The hybrid may take the structure-preserving path (no warnings) or the
    // flatten fallback (a "flattened" note) depending on how OpenUSD handles
    // the dropped reference arc — either is acceptable; what matters is that
    // the mesh is not lost and the source is untouched.
    let mut pkg = StageService::open(&dest).expect("reopen packaged usdz");
    let recs = pkg.project_topology().expect("packaged topology");
    assert!(
        recs.iter()
            .any(|r| r.type_name == "Mesh" && r.name == "Body"),
        "the local Body mesh must survive packaging; got {} prims",
        recs.len()
    );

    std::fs::remove_file(&dest).ok();
    std::fs::remove_file(&baseline_dest).ok();
}

/// Divergent relative spellings of the SAME file must not drop geometry.
/// `divergent_spelling/Assets/scene/scene.usda` composes with ZERO unresolved
/// references, but references `widget.usda` two ways — `../widget/...` from the
/// root layer and `../../widget/...` from a `stage/` sublayer. Without the
/// canonicalization pre-pass `OpenUSD`'s package directory remapper gives each
/// spelling its own bucket and leaves one empty, dangling one arc and dropping
/// one of the two `WidgetMesh` prims. `package_stage` (the path a fully
/// resolvable source takes) must now preserve BOTH, and leave the source
/// untouched. This is the minimal reproduction of the BarScene instancing loss.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn divergent_spellings_do_not_drop_geometry() {
    let source = fixture("layering/divergent_spelling/Assets/scene/scene.usda");
    let before = std::fs::read(&source).expect("read fixture");

    let dest = std::env::temp_dir().join(format!(
        "idtx-divergent-{}-{}.usdz",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));

    // The source is fully resolvable, so this takes the plain package_stage
    // path (no unresolved refs to drop) and must succeed with a written file.
    let result = package_stage(&source, &dest, PackagingPurpose::PreserveAuthoring)
        .expect("packaging should succeed");
    assert!(dest.is_file(), "a package file must be written");
    assert_eq!(
        result.destination, dest,
        "result should report the destination"
    );
    assert!(
        result.unresolved.is_empty(),
        "the source resolves cleanly — nothing should be reported unresolved"
    );

    // The source file on disk must be byte-for-byte identical (the pre-pass
    // edits only in-memory layer copies and reverts them).
    let after = std::fs::read(&source).expect("re-read fixture");
    assert_eq!(before, after, "source fixture must be left untouched");

    // The crux: BOTH arcs to widget.usda survive. Without canonicalization one
    // bucket is empty and only a single WidgetMesh composes.
    let mut pkg = StageService::open(&dest).expect("reopen packaged usdz");
    let recs = pkg.project_topology().expect("packaged topology");
    let widget_meshes = recs
        .iter()
        .filter(|r| r.type_name == "Mesh" && r.name == "WidgetMesh")
        .count();
    assert_eq!(
        widget_meshes, 2,
        "both divergent-spelling references must resolve to the packaged widget \
             (got {widget_meshes} of 2 WidgetMesh prims; a dangling bucket dropped one)"
    );

    std::fs::remove_file(&dest).ok();
}

/// Loose staging of a dependency closure must preserve directory structure
/// AND composition/instancing byte-for-byte (no remapping), unlike the
/// `.usdz` flatten path. Divergent-spelling fixture: `scene.usda` references
/// `widget.usda` two ways (`../widget` and a `stage/` sublayer's
/// `../../widget`); both must still resolve in the staged tree → two
/// `WidgetMesh` prims.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn stage_loose_closure_preserves_structure_and_instancing() {
    let source = fixture("layering/divergent_spelling/Assets/scene/scene.usda");
    let dest = std::env::temp_dir().join(format!(
        "idtx-loose-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dest).unwrap();

    let result = stage_loose_closure(&source, &dest).expect("stage loose closure");

    // The entry stages under its path relative to the closure common root
    // (`.../Assets`), i.e. `scene/scene.usda`.
    let entry = dest.join(&result.staged_entry);
    assert!(entry.is_file(), "staged entry exists: {}", entry.display());
    assert!(dest.join("scene/scene.usda").is_file(), "root layer staged");
    assert!(
        dest.join("scene/stage/sub.usda").is_file(),
        "sublayer staged"
    );
    assert!(
        dest.join("widget/widget.usda").is_file(),
        "referenced asset staged"
    );

    // Composition + instancing preserved: BOTH divergent-spelling arcs resolve
    // in the staged tree with no rewriting → two WidgetMesh prims.
    let mut staged = StageService::open(&entry).expect("reopen staged tree");
    let recs = staged.project_topology().expect("staged topology");
    let widget_meshes = recs
        .iter()
        .filter(|r| r.type_name == "Mesh" && r.name == "WidgetMesh")
        .count();
    assert_eq!(
        widget_meshes, 2,
        "both references must resolve in the loose-staged tree (got {widget_meshes})"
    );

    std::fs::remove_dir_all(&dest).ok();
}

/// Loose staging must keep a remote (`http(s)://`) reference EXTERNAL
/// verbatim — never copy it, never canonicalize the URL as a filesystem path
/// (which fails with "No such file or directory"). This is the regression
/// test for the desktop-build "loose staging failed: resolve https://… No
/// such file or directory" crash: the loose path never got the remote-ref
/// handling the usdz packager has. No HTTP resolver is installed, so the
/// remote URL is unresolvable (the packaging-mode condition the cook arms);
/// the local closure still stages and the remote URL surfaces in `external`.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn stage_loose_keeps_remote_ref_external_not_canonicalized() {
    const REMOTE_URL: &str = "https://assets.example.invalid/pack/remote.usda";

    let dir = std::env::temp_dir().join(format!(
        "idtx-loose-remote-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join("stages")).expect("create temp dir");
    std::fs::create_dir_all(dir.join("assets")).expect("create assets dir");

    // A local asset referenced layer-relative (stays in the project root →
    // copied into the loose tree) plus a remote payload (kept external).
    std::fs::write(
            dir.join("assets/widget.usda"),
            "#usda 1.0\n(\n    defaultPrim = \"Widget\"\n)\ndef Xform \"Widget\"\n{\n    def Mesh \"LocalMesh\" {}\n}\n",
        )
        .expect("write local asset");
    let entry = dir.join("stages/s.usda");
    std::fs::write(
            &entry,
            format!(
                "#usda 1.0\n(\n    defaultPrim = \"root\"\n)\n\
                 def Xform \"root\"\n{{\n\
                 \x20\x20def \"local\" (\n\x20\x20\x20\x20prepend references = @../assets/widget.usda@</Widget>\n\x20\x20) {{}}\n\
                 \x20\x20def \"remote\" (\n\x20\x20\x20\x20prepend payload = @{REMOTE_URL}@\n\x20\x20) {{}}\n}}\n",
            ),
        )
        .expect("write entry");

    let dest = std::env::temp_dir().join(format!(
        "idtx-loose-remote-out-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dest).expect("create dest");

    // Stage relative to the project root (`dir`), as the cook does. This must
    // NOT error on the remote ref.
    let result = stage_loose_closure_under_root(&entry, &dest, &dir)
        .expect("loose staging keeps remote external");

    // Local closure staged under its project-relative path.
    assert!(
        dest.join("stages/s.usda").is_file(),
        "entry layer staged into the loose tree"
    );
    assert!(
        dest.join("assets/widget.usda").is_file(),
        "local referenced asset staged into the loose tree"
    );
    // Remote URL surfaced external, never copied / canonicalized.
    assert!(
        result
            .external
            .iter()
            .any(|p| p.to_str().is_some_and(|s| s.contains("remote.usda"))),
        "remote URL must be reported external; got {:?}",
        result.external
    );

    std::fs::remove_dir_all(&dest).ok();
    std::fs::remove_dir_all(&dir).ok();
}

// must package into a self-contained usdz with every referenced asset
// localized — this is the regression guard for the "Save As… → usdz" bug
// where exporting the root layer into an unrelated temp directory left the
// relative references unresolvable and produced no package at all.

#[test]
fn packaging_loose_scene_localizes_relative_references() {
    let source = fixture("authoring/pack_referenced/root.usda");
    let stage = StageService::open(&source).expect("open referenced scene");
    let destination = std::env::temp_dir().join(format!(
        "idtx-pack-referenced-{}-{}.usdz",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));

    let result = stage
        .package_loose_as_usdz(&source, &destination)
        .expect("package loose scene into usdz");

    assert!(result.unresolved.is_empty(), "no assets left unresolved");
    // A clean, fully-resolvable stage packages without diagnostics — the
    // Conductor "Import" flow relies on empty `warnings` meaning "close the
    // modal and open immediately" (spec case 3, no-warnings path).
    assert!(
        result.warnings.is_empty(),
        "clean package emitted diagnostics: {:?}",
        result.warnings
    );
    assert!(destination.is_file(), "usdz package written to disk");
    assert!(
        result
            .dependencies
            .iter()
            .any(|dependency| dependency.ends_with("child.usda")),
        "referenced child layer localized: {:?}",
        result.dependencies
    );
    assert!(
        result
            .dependencies
            .iter()
            .any(|dependency| dependency.ends_with("dot.png")),
        "referenced texture localized: {:?}",
        result.dependencies
    );

    // Reopening the package must resolve the reference from inside the usdz.
    let packaged = StageService::open(&destination).expect("reopen usdz package");
    assert!(
        packaged.prim_at_path("/World/Ref/M").is_some(),
        "referenced geometry present inside the package"
    );
    std::fs::remove_file(&destination).expect("remove package");
}

// A dangling reference must surface as an error, not a silent success that
// reports "Saved" while writing no file.

#[test]
fn packaging_loose_scene_with_missing_asset_errors() {
    let work = std::env::temp_dir().join(format!(
        "idtx-pack-missing-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&work).expect("mkdir work");
    let source = work.join("root.usda");
    std::fs::write(
            &source,
            "#usda 1.0\ndef Xform \"World\"\n{\n    def \"Ref\" (\n        prepend references = @./does_not_exist.usda@\n    ) {}\n}\n",
        )
        .expect("write root");
    let stage = StageService::open(&source).expect("open scene with missing asset");
    let destination = work.join("out.usdz");

    let result = stage.package_loose_as_usdz(&source, &destination);

    assert!(
        matches!(result, Err(StageError::UnresolvedAssets(_))),
        "missing asset reported as an error, got {result:?}"
    );
    assert!(!destination.is_file(), "no package written on failure");
    let _ = std::fs::remove_dir_all(&work);
}

// An in-place Save of a transform edit must succeed even when the source
// stage carries a pre-existing *unresolvable* reference (BarScene's
// `reference_napkins.usd` authored anonymous-layer refs + missing textures).
// The dependency gate is a no-regression check, not a completeness gate: if
// the stage loads (usdview composes it, warnings and all), the edit persists.

#[test]
fn save_persists_transform_edit_despite_unresolved_reference() {
    let src = fixture("authoring/save_with_unresolved_reference.usda");
    // Copy into a private temp dir so the (unresolvable) relative reference
    // stays broken and the in-place save target is isolated.
    let work = std::env::temp_dir().join(format!(
        "idtx-save-unresolved-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&work).expect("mkdir work");
    let dest = work.join("scene.usda");
    std::fs::copy(&src, &dest).expect("copy fixture");

    let mut stage = StageService::open(&dest).expect("open stage with broken reference");
    stage.project_topology().expect("topology");
    let prim = stage.prim_at_path("/bar_scene").expect("bar_scene prim");

    stage
        .apply_transform_edit(
            prim,
            TransformValues {
                translation: [1.0, 2.0, 3.0],
                rotation_xyz_degrees: [0.0, 0.0, 0.0],
                scale: [42.0, 42.0, 42.0],
            },
        )
        .expect("author new scale");

    // This is the regression: previously `Err(... "unresolved dependencies
    // ... cannot be verified for save")`.
    stage
        .save_stage(&dest)
        .expect("save tolerates pre-existing unresolved reference");

    drop(stage);

    // Reopen from disk and confirm the new scale was actually written.
    let mut reopened = StageService::open(&dest).expect("reopen saved stage");
    reopened.project_topology().expect("topology reopen");
    let prim = reopened
        .prim_at_path("/bar_scene")
        .expect("bar_scene reopen");
    let mat = reopened
        .get_local_transform(prim)
        .expect("get_local_transform");
    let sx = (mat[0][0] * mat[0][0] + mat[0][1] * mat[0][1] + mat[0][2] * mat[0][2]).sqrt();
    assert!(
        (sx - 42.0_f32).abs() < 1e-3,
        "saved scale must persist to disk; got Sx={sx}"
    );

    let _ = std::fs::remove_dir_all(&work);
}

fn isolated_fixture(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "idtx-{label}-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::copy(fixture("scene/simple_hierarchy.usda"), &path).expect("copy isolated fixture");
    path
}

#[test]
fn stage_projects_bulk_topology_viewport_and_live_relationship_targets() {
    let mut stage =
        StageService::open(fixture("scene/simple_hierarchy.usda")).expect("open fixture");
    assert!(stage.topology()[0].is_pseudo_root);
    assert_eq!(stage.metrics().topology.calls, 1);
    let viewport = stage.project_viewport().expect("viewport projection");
    assert_eq!(viewport.cubes.len(), 1);
    assert_eq!(stage.metrics().viewport.calls, 1);
    let group = stage.prim_for_path("/World/Group").expect("group");
    let properties = stage.inspect_prim(group).expect("inspection");
    let relationship = properties
        .iter()
        .find(|property| property.name == "lookAt")
        .expect("relationship");
    assert_eq!(relationship.relationship_targets, ["/World/Cube"]);
    assert_eq!(relationship.value_source, "authored targets");
    assert!(!relationship.provenance.is_empty());
    let cube = stage.prim_for_path("/World/Group/Cube").expect("cube");
    let properties = stage.inspect_prim(cube).expect("cube inspection");
    let double_sided = properties
        .iter()
        .find(|property| property.name == "doubleSided")
        .expect("schema-defined default");
    assert!(!double_sided.authored);
    assert_eq!(double_sided.value_source, "schema fallback");
}

#[test]
fn viewport_projects_typed_lights_and_reports_explicit_quality_limits() {
    let mut stage = StageService::open(fixture("lighting/lights.usda")).expect("open lights");
    let projection = stage.project_viewport().expect("project lights");
    assert_eq!(projection.schema_version, PROJECTION_SCHEMA_VERSION);
    // DiskLight and CylinderLight are now projected as supported lights
    // (previously dropped as unsupported), raising the count from 6 to 8.
    assert_eq!(projection.lights.len(), 8);

    let key_prim = stage
        .prim_for_path("/World/Supported/Key")
        .expect("key light");
    let key = projection
        .lights
        .iter()
        .find(|light| light.prim == key_prim)
        .expect("projected key light");
    assert!(matches!(
        key.kind,
        LightKind::Distant { angle } if (angle - 0.25).abs() < f32::EPSILON
    ));
    assert!((key.intensity - 12.5).abs() < f32::EPSILON);
    assert!((key.exposure - 2.0).abs() < f32::EPSILON);
    assert!((key.diffuse - 0.75).abs() < f32::EPSILON);
    assert!((key.specular - 0.4).abs() < 1.0e-6);
    assert!(key.normalize);
    assert!(key.shadow_enabled);
    assert!((key.world_matrix[12] - 11.0).abs() < 1.0e-5);
    assert!((key.world_matrix[13] - 22.0).abs() < 1.0e-5);
    assert!((key.world_matrix[14] - 33.0).abs() < 1.0e-5);
    for (actual, expected) in key
        .color
        .iter()
        .zip([0.885_514_2, 0.211_073_38, 0.034_003_97])
    {
        assert!((actual - expected).abs() < 1.0e-6);
    }

    let bounce = projection
        .lights
        .iter()
        .find(|light| stage.path_for_prim(light.prim).ok() == Some("/World/Supported/Bounce"))
        .expect("projected sphere light");
    assert!(matches!(
        bounce.kind,
        LightKind::Sphere { radius } if (radius - 0.75).abs() < f32::EPSILON
    ));
    assert!(bounce.shadow_enabled);
    let fill = projection
        .lights
        .iter()
        .find(|light| stage.path_for_prim(light.prim).ok() == Some("/World/Supported/Fill"))
        .expect("projected rect light");
    assert!(matches!(
        fill.kind,
        LightKind::Rect { width, height }
            if (width - 2.0).abs() < f32::EPSILON
                && (height - 3.0).abs() < f32::EPSILON
    ));
    assert!(
        projection.lights.iter().all(|light| {
            stage.path_for_prim(light.prim).ok().as_deref() != Some("/World/Hidden")
        })
    );

    // DiskLight and CylinderLight now project as first-class lights.
    assert!(
        projection
            .lights
            .iter()
            .any(|light| matches!(light.kind, LightKind::Disk { .. })),
        "disk light now projected as supported"
    );
    assert!(
        projection
            .lights
            .iter()
            .any(|light| matches!(light.kind, LightKind::Cylinder { .. })),
        "cylinder light now projected as supported"
    );

    for unsupported in ["PortalLight", "MeshLight", "GeometryLight", "PluginLight"] {
        assert!(
            projection
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains(unsupported)),
            "missing diagnostic for {unsupported}"
        );
    }
    // Six supported lights sit under the raised MAX_LIGHTS = 16 cap, so no
    // light-count limit diagnostic should be emitted.
    assert!(
        !projection
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.contains("supports at most 16 lights") })
    );
    assert!(
        projection
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.contains("only DistantLight shadows are supported") })
    );
    let shadow_limit = projection
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.contains("one DistantLight")
                && diagnostic.contains("1024x1024")
                && diagnostic.contains("PCF")
        })
        .count();
    assert_eq!(shadow_limit, 1);
}

#[test]
fn camera_projection_preserves_complete_authored_state_and_selection_policy() {
    let mut stage =
        StageService::open(fixture("scene/camera_bounds.usda")).expect("open camera fixture");
    let projection = stage.project_viewport().expect("project cameras");
    assert_eq!(projection.cameras.len(), 2);
    assert_eq!(
        projection.camera_selection.policy,
        CameraSelectionPolicy::FirstInStageTraversal
    );
    assert_eq!(projection.camera_selection.authored_camera_count, 2);
    let camera = projection.cameras.first().expect("selected camera record");
    assert_eq!(projection.camera_selection.selected, Some(camera.prim));
    assert_eq!(stage.path_for(camera.prim), Some("/World/FirstCamera"));
    let close = |actual: f64, expected: f64| {
        assert!((actual - expected).abs() < 1.0e-6, "{actual} != {expected}");
    };
    close(camera.horizontal_aperture_offset, 1.25);
    close(camera.vertical_aperture_offset, -0.75);
    for (actual, expected) in camera.clipping_planes[0]
        .into_iter()
        .zip([1.0, 0.0, 0.0, -100.0])
    {
        close(actual, expected);
    }
    close(camera.f_stop, f64::from(2.8_f32));
    close(camera.focus_distance, 750.0);
    assert_eq!(camera.stereo_role, "left");
    close(camera.shutter[0], -0.25);
    close(camera.shutter[1], 0.25);
    close(camera.exposure, 1.5);
    close(camera.exposure_iso, 200.0);
    close(camera.exposure_time, 0.5);
    close(camera.exposure_f_stop, 4.0);
    close(camera.exposure_responsivity, f64::from(0.8_f32));
    assert!(projection.camera_selection.diagnostic.is_some());
    assert!(projection.diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("first-in-stage-traversal") && diagnostic.contains("FirstCamera")
    }));
    assert!(projection.diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("aperture offsets") && diagnostic.contains("physical exposure")
    }));
}

#[test]
fn inspection_reports_composed_variants_and_applied_apis() {
    let mut variants = StageService::open(fixture("scene/variants.usda")).expect("open variants");
    let world = variants.prim_for_path("/World").expect("world");
    let sets = variants.variant_sets(world).expect("inspect variants");
    let selection = sets
        .iter()
        .find(|set| set.name == "shape")
        .expect("variant selection");
    assert_eq!(selection.selection, "Cube");
    assert_eq!(selection.choices, ["Cube", "Sphere"]);

    let mut material = StageService::open(fixture("materials/preview_surface.usda"))
        .expect("open material fixture");
    let world = material.prim_for_path("/World").expect("world");
    let properties = material.inspect_prim(world).expect("inspect APIs");
    let schemas = properties
        .iter()
        .find(|property| property.name == "apiSchemas")
        .expect("applied schemas");
    assert_eq!(schemas.kind, PropertyKind::Metadata);
    assert!(schemas.value_summary.contains("MaterialBindingAPI"));
}

#[test]
fn session_variant_changes_composition_without_modifying_the_root_layer() {
    let fixture = fixture("scene/variants.usda");
    let before = std::fs::read(&fixture).expect("read root before");
    {
        let mut stage = StageService::open(&fixture).expect("open variants");
        let world = stage.prim_for_path("/World").expect("world");
        stage
            .set_session_variant(world, "shape", "Sphere")
            .expect("set session variant");
        let batch = stage
            .drain_changes()
            .expect("drain variant notice")
            .expect("variant change batch");
        assert!(batch.changes.iter().any(|change| change.resync));
        stage.project_topology().expect("reproject topology");
        assert!(stage.prim_at_path("/World/Geometry").is_some());
        let sets = stage.variant_sets(world).expect("variant sets");
        assert_eq!(sets[0].selection, "Sphere");
        assert!(
            sets[0]
                .provenance
                .iter()
                .any(|source| source.contains("session"))
        );
    }
    assert_eq!(std::fs::read(&fixture).expect("read root after"), before);
    let mut reopened = StageService::open(&fixture).expect("reopen root");
    let world = reopened.prim_for_path("/World").expect("world");
    assert_eq!(
        reopened.variant_sets(world).expect("variant sets")[0].selection,
        "Cube"
    );
}

#[test]
#[ignore]
fn viewport_projection_preserves_material_subset_face_assignments() {
    let mut stage = StageService::open(fixture("materials/material_subsets.usda"))
        .expect("open subset fixture");
    let projection = stage.project_viewport().expect("viewport projection");
    let mesh = projection.meshes.first().expect("mesh");
    assert_eq!(mesh.material_subsets.len(), 2);
    assert_eq!(mesh.material_subsets[0].face_indices, [0]);
    assert_eq!(mesh.material_subsets[1].face_indices, [1]);
    assert!(
        projection
            .materials
            .iter()
            .any(|material| material.path == "/World/Looks/Warm")
    );
    assert!(
        projection
            .materials
            .iter()
            .any(|material| material.path == "/World/Looks/Cool")
    );
    let mesh_prim = stage.prim_for_path("/World/Panel").expect("mesh prim");
    let graph = stage
        .material_graph(mesh_prim)
        .expect("subset material graph");
    assert_eq!(graph.material_paths.len(), 3);
    assert!(
        graph
            .material_paths
            .iter()
            .any(|path| path == "/World/Looks/Neutral")
    );
    assert!(
        graph
            .material_paths
            .iter()
            .any(|path| path == "/World/Looks/Warm")
    );
    assert!(
        graph
            .material_paths
            .iter()
            .any(|path| path == "/World/Looks/Cool")
    );
    assert_eq!(graph.nodes.len(), 3);
}

// Regression: a UsdUVTexture bound to diffuseColor carries a `scale` factor
// (spec: result = sample*scale + bias) that tints the sampled texture. Its
// scale.rgb must survive as `base_color` (applied as a multiplicative tint
// by the shader) instead of being normalised to white. A texture with no
// authored scale must still normalise to white so a plain texture is
// unaffected. This is how per-GeomSubset colour variants sharing one texture
// (e.g. the TextureCoordinateTest asset) express their distinct corner tints.
#[test]
fn material_projection_applies_texture_scale_as_base_color_tint() {
    let mut stage = StageService::open(fixture("materials/subset_tinted_texture.usda"))
        .expect("open tinted-subset fixture");
    let projection = stage.project_viewport().expect("viewport projection");
    let material = |path: &str| {
        projection
            .materials
            .iter()
            .find(|m| m.path == path)
            .unwrap_or_else(|| panic!("material {path} projected"))
    };

    let warm = material("/World/Looks/Warm");
    assert!(warm.diffuse_texture.is_some(), "Warm binds the shared texture");
    assert!(
        warm.base_color
            .iter()
            .zip([0.8, 0.3, 0.1])
            .all(|(a, b)| (a - b).abs() < 1e-5),
        "Warm keeps its UsdUVTexture scale as the diffuse tint, got {:?}",
        warm.base_color
    );

    let cool = material("/World/Looks/Cool");
    assert!(cool.diffuse_texture.is_some(), "Cool binds the shared texture");
    assert!(
        cool.base_color
            .iter()
            .zip([0.1, 0.3, 0.8])
            .all(|(a, b)| (a - b).abs() < 1e-5),
        "Cool keeps its UsdUVTexture scale as the diffuse tint, got {:?}",
        cool.base_color
    );

    let plain = material("/World/Looks/TextureOnly");
    assert!(plain.diffuse_texture.is_some(), "TextureOnly binds the texture");
    assert_eq!(
        plain.base_color,
        [1.0, 1.0, 1.0],
        "texture with no authored scale normalises base_color to white"
    );
}

#[test]
fn topology_growth_preserves_existing_ids_and_maps_new_notice_to_known_root() {
    let isolated = isolated_fixture("topology-growth");
    {
        let mut stage = StageService::open(&isolated).expect("open fixture");
        let world_before = stage.prim_for_path("/World").expect("world before");
        stage
            .conformance_define_xform("/Aardvark")
            .expect("define earlier-sorting prim");
        let batch = stage.drain_changes().expect("drain").expect("change batch");
        assert!(batch.changes.iter().any(|change| change.resync));
        stage.project_topology().expect("refresh topology");
        assert_eq!(
            stage.prim_for_path("/World").expect("world after"),
            world_before
        );
        assert!(stage.prim_for_path("/Aardvark").is_ok());
    }
    std::fs::remove_file(isolated).expect("remove isolated fixture");
}

#[test]
fn reopening_allocates_a_new_generation() {
    let first = StageService::open(fixture("scene/simple_hierarchy.usda")).expect("first");
    let second = StageService::open(fixture("scene/simple_hierarchy.usda")).expect("second");
    assert_ne!(first.generation(), second.generation());
}

#[test]
fn invalid_root_layer_returns_without_lifecycle_deadlock() {
    assert!(StageService::open(fixture("scene/invalid_metadata.usda")).is_err());
}

#[test]
fn concurrent_same_layer_open_projection_and_drop_is_stable() {
    const WORKERS: usize = 8;
    let stage_path = Arc::new(fixture("scene/poser_transform.usda"));
    let barrier = Arc::new(Barrier::new(WORKERS));
    std::thread::scope(|scope| {
        for worker in 0..WORKERS {
            let stage_path = Arc::clone(&stage_path);
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                barrier.wait();
                if worker < WORKERS / 2 {
                    let mut stage = StageService::open(stage_path.as_ref()).expect("reader open");
                    for _ in 0..32 {
                        stage.project_viewport().expect("reader projection");
                    }
                } else {
                    for _ in 0..32 {
                        let stage =
                            StageService::open(stage_path.as_ref()).expect("lifecycle open");
                        drop(stage);
                    }
                }
            });
        }
    });
}

#[test]
fn unknown_purpose_is_a_visible_diagnostic_not_a_projection_failure() {
    let mut stage = StageService::open(fixture("scene/unknown_purpose.usda")).expect("open stage");
    let projection = stage.project_viewport().expect("project stage");
    assert!(
        projection
            .visibility
            .iter()
            .any(|record| record.purpose == RenderPurpose::Default)
    );
    assert!(
        projection
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("unknown imageable purpose other"))
    );
}

#[test]
fn batched_animation_sample_uses_host_stage_time() {
    let mut stage = StageService::open(fixture("skinning/animation.usda")).expect("open animation");
    let animated = stage
        .prim_for_path("/World/Animated")
        .expect("animated cube");
    let at = |projection: &ViewportProjection| {
        projection
            .transforms
            .iter()
            .find(|record| record.prim == animated)
            .expect("animated transform")
            .world_matrix[12]
    };
    let start = stage.sample_viewport(0.0).expect("sample start");
    assert_eq!(start.time_code, Some(0.0));
    assert!((at(&start) + 2.0).abs() < 1.0e-5);
    let middle = stage.sample_viewport(24.0).expect("sample middle");
    assert!((at(&middle) - 2.0).abs() < 1.0e-5);
    assert!(
        !middle
            .visibility
            .iter()
            .find(|record| record.prim == animated)
            .expect("animated visibility")
            .visible
    );
}

#[test]
fn nested_reference_animation_obeys_composed_layer_offset() {
    let mut stage =
        StageService::open(fixture("skinning/packed_host.usda")).expect("open packed host");
    let body = stage
        .prim_for_path("/World/Packed/ReferencedBody")
        .expect("referenced body");
    let end = stage.sample_viewport(24.0).expect("sample host end");
    let matrix = end
        .transforms
        .iter()
        .find(|record| record.prim == body)
        .expect("referenced transform")
        .world_matrix;
    assert!(
        (matrix[12] - 4.0).abs() < 1.0e-5,
        "host t=24 must map to referenced t=48"
    );
}

#[test]
fn two_joint_skinning_projects_official_pose_palette_and_world_bounds() {
    fn transform_point(matrix: &[f32; 16], point: [f32; 3]) -> [f32; 3] {
        [
            point[0].mul_add(
                matrix[0],
                point[1].mul_add(matrix[4], point[2].mul_add(matrix[8], matrix[12])),
            ),
            point[0].mul_add(
                matrix[1],
                point[1].mul_add(matrix[5], point[2].mul_add(matrix[9], matrix[13])),
            ),
            point[0].mul_add(
                matrix[2],
                point[1].mul_add(matrix[6], point[2].mul_add(matrix[10], matrix[14])),
            ),
        ]
    }

    let mut stage =
        StageService::open(fixture("skinning/skinning_two_joint.usda")).expect("open skin fixture");
    let skeleton = stage
        .prim_for_path("/World/Rig/Skeleton")
        .expect("skeleton");
    let animation = stage
        .prim_for_path("/World/Rig/Animation")
        .expect("animation");
    let mesh_prim = stage.prim_for_path("/World/Rig/Mesh").expect("mesh");

    let mut sampled_bounds = Vec::new();
    for time in [0.0, 1.0] {
        let projection = stage.sample_viewport(time).expect("sample skin fixture");
        let topology = projection
            .skeleton_topologies
            .iter()
            .find(|record| record.skeleton == skeleton)
            .expect("typed skeleton topology");
        assert_eq!(topology.joints.len(), 2);
        assert_eq!(topology.joints[0].parent, None);
        assert_eq!(topology.joints[1].parent, Some(0));
        let pose = projection
            .skeleton_poses
            .iter()
            .find(|record| record.skeleton == skeleton)
            .expect("sampled skeleton pose");
        assert_eq!(pose.animation_source, Some(animation));
        assert_eq!(pose.skinning_transforms.len(), 2);
        assert!((pose.skeleton_world_transform[12] - 10.0).abs() < 1.0e-5);
        assert!((pose.skeleton_world_transform[13] - 5.0).abs() < 1.0e-5);
        let binding = projection
            .skin_bindings
            .iter()
            .find(|record| record.prim == mesh_prim)
            .expect("typed mesh skin binding");
        assert_eq!(binding.skeleton, skeleton);
        assert_eq!(binding.method, SkinningMethod::ClassicLinear);
        assert_eq!(binding.execution, SkinningExecution::GpuClassicLinear);
        assert!((binding.geom_bind_transform[12] - 1.0).abs() < 1.0e-5);
        assert!((binding.geom_bind_transform[13] - 2.0).abs() < 1.0e-5);
        let mesh = projection
            .meshes
            .iter()
            .find(|record| record.prim == mesh_prim)
            .expect("skinned mesh");
        assert_eq!(binding.influences.len(), mesh.points.len());

        let mut expected_minimum = [f32::INFINITY; 3];
        let mut expected_maximum = [f32::NEG_INFINITY; 3];
        for (point, influences) in mesh.points.iter().zip(&binding.influences) {
            let bound = transform_point(&binding.geom_bind_transform, *point);
            let mut deformed = [0.0; 3];
            for (joint, weight) in influences.joints.iter().zip(influences.weights) {
                let transformed =
                    transform_point(&pose.skinning_transforms[usize::from(*joint)], bound);
                for axis in 0..3 {
                    deformed[axis] = transformed[axis].mul_add(weight, deformed[axis]);
                }
            }
            let world = transform_point(&pose.skeleton_world_transform, deformed);
            for axis in 0..3 {
                expected_minimum[axis] = expected_minimum[axis].min(world[axis]);
                expected_maximum[axis] = expected_maximum[axis].max(world[axis]);
            }
        }
        let official = binding
            .skinned_bounds
            .expect("official OpenUSD skinned bounds");
        for (actual, expected) in official.minimum.into_iter().zip(expected_minimum) {
            assert!((actual - expected).abs() < 1.0e-4);
        }
        for (actual, expected) in official.maximum.into_iter().zip(expected_maximum) {
            assert!((actual - expected).abs() < 1.0e-4);
        }
        sampled_bounds.push(official);
    }
    assert_ne!(sampled_bounds[0], sampled_bounds[1]);

    let rooted = stage
        .sample_viewport_roots(1.0, &[animation])
        .expect("rooted animation sample");
    assert!(rooted.meshes.iter().any(|record| record.prim == mesh_prim));
    assert!(
        rooted
            .skin_bindings
            .iter()
            .any(|record| record.prim == mesh_prim)
    );
    assert!(
        rooted
            .skeleton_topologies
            .iter()
            .any(|record| record.skeleton == skeleton)
    );
    assert_eq!(rooted.skeleton_dependencies[0].skinned_prims, [mesh_prim]);
}

#[test]
fn skeleton_animation_and_relationship_changes_invalidate_bound_meshes() {
    let isolated = std::env::temp_dir().join(format!(
        "idtx-skinning-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::copy(fixture("skinning/skinning_two_joint.usda"), &isolated)
        .expect("copy isolated skin fixture");
    let mut stage = StageService::open(&isolated).expect("open isolated skin fixture");
    let animation = stage
        .prim_for_path("/World/Rig/Animation")
        .expect("animation");
    let alternate = stage
        .prim_for_path("/World/Rig/AnimationAlt")
        .expect("alternate animation");
    let rig = stage.prim_for_path("/World/Rig").expect("rig");
    let mesh = stage.prim_for_path("/World/Rig/Mesh").expect("mesh");
    let before = stage
        .sample_viewport(1.0)
        .expect("initial sample")
        .skin_bindings[0]
        .skinned_bounds
        .expect("initial skinned bounds");

    stage
        .conformance_author_vec3f_array_at_time(
            "/World/Rig/Animation.translations",
            &[[0.0, 0.0, 0.0], [4.0, 1.0, 0.0]],
            1.0,
        )
        .expect("author animation sample");
    let animation_batch = stage
        .drain_changes()
        .expect("drain animation change")
        .expect("animation batch");
    assert!(animation_batch.changes.iter().any(|change| {
        change.prim == animation
            && change.domains.contains(DirtyDomains::ANIMATION)
            && change.domains.contains(DirtyDomains::SKELETON)
    }));
    let animation_rooted = stage
        .sample_viewport_roots(1.0, &[animation])
        .expect("rooted changed animation sample");
    let changed_binding = animation_rooted
        .skin_bindings
        .iter()
        .find(|binding| binding.prim == mesh)
        .expect("bound mesh expanded from animation root");
    assert_ne!(changed_binding.skinned_bounds, Some(before));

    stage
        .conformance_set_relationship_target(
            "/World/Rig.skel:animationSource",
            "/World/Rig/AnimationAlt",
        )
        .expect("switch animation source");
    let relationship_batch = stage
        .drain_changes()
        .expect("drain relationship change")
        .expect("relationship batch");
    assert!(relationship_batch.changes.iter().any(|change| {
        change.prim == rig
            && change.domains.contains(DirtyDomains::RELATIONSHIP)
            && change.domains.contains(DirtyDomains::SKELETON)
    }));
    let relationship_rooted = stage
        .sample_viewport_roots(1.0, &[rig])
        .expect("rooted changed relationship sample");
    assert!(
        relationship_rooted
            .skin_bindings
            .iter()
            .any(|binding| binding.prim == mesh)
    );
    assert_eq!(
        relationship_rooted.skeleton_dependencies[0].animation_source,
        Some(alternate)
    );
    assert_eq!(
        relationship_rooted.skeleton_poses[0].animation_source,
        Some(alternate)
    );
    std::fs::remove_file(isolated).expect("remove isolated skin fixture");
}

#[test]
fn topology_only_open_loads_a_payload_on_explicit_request() {
    let mut stage = StageService::open_topology_only(fixture("hydration/payload_host.usda"))
        .expect("open topology only");
    let payload = stage
        .prim_for_path("/World/Payload")
        .expect("payload boundary");
    let row = stage
        .topology()
        .iter()
        .find(|row| row.prim == payload)
        .expect("payload row");
    assert!(!row.state.loaded);
    assert!(
        stage
            .prim_for_path("/World/Payload/ReferencedBody")
            .is_err()
    );

    stage
        .set_payload_loaded(payload, true)
        .expect("load payload");
    stage.drain_changes().expect("drain payload resync");
    stage.project_topology().expect("refresh topology");
    assert!(stage.prim_for_path("/World/Payload/ReferencedBody").is_ok());
}

#[test]
fn level_transition_assembly_holders_toggle_payload_and_visibility() {
    // Validates the assembly fixture without a GPU: level holders under
    // /World/Levels carry the authored starting visibility the transition
    // manager expects, and their payloads load/unload on request.
    let mut stage =
        StageService::open_topology_only(layerstack_fixture("level_transition_assembly.usda"))
            .expect("open assembly topology only");

    let hub = stage.prim_at_path("/World/Levels/Hub").expect("Hub holder");
    let forest = stage
        .prim_at_path("/World/Levels/Forest")
        .expect("Forest holder");

    // Authored starting visibility baked into the fixture.
    assert!(
        stage.prim_authored_visible(hub).expect("hub visibility"),
        "Hub is authored visible (first-loaded level)"
    );
    assert!(
        !stage
            .prim_authored_visible(forest)
            .expect("forest visibility"),
        "Forest is authored invisible until transitioned to"
    );

    // Under LoadNone the level payloads are not composed.
    assert!(
        stage.prim_at_path("/World/Levels/Forest/Body").is_none(),
        "Forest payload starts unloaded"
    );

    // Loading the payload composes the level's content under the holder.
    stage
        .set_payload_loaded(forest, true)
        .expect("load Forest payload");
    stage.drain_changes().expect("drain load resync");
    stage.project_topology().expect("refresh topology");
    assert!(
        stage.prim_at_path("/World/Levels/Forest/Body").is_some(),
        "Forest payload composed after load"
    );

    // Unloading reclaims it again — the round-trip the manager relies on.
    stage
        .set_payload_loaded(forest, false)
        .expect("unload Forest payload");
    stage.drain_changes().expect("drain unload resync");
    stage.project_topology().expect("refresh topology");
    assert!(
        stage.prim_at_path("/World/Levels/Forest/Body").is_none(),
        "Forest payload reclaimed after unload"
    );
}

#[test]
fn subtree_reprojection_never_duplicates_prototype_records() {
    // Regression: a subtree `project_topology_paths` must never leave a
    // duplicated or unflagged copy of a shared instancing prototype in the
    // topology store.
    //
    // Two failure modes are guarded here, both observed with the instanced
    // Kitchen_set drop (total=3989, is_prototype=1766, __Prototype-by-path
    // =3532 — i.e. every prototype prim stored TWICE, half unflagged):
    //   1. A resync hands `project_topology_paths` a `/__Prototype_N` root
    //      (payload load composes new prototypes). The shim's per-root walk
    //      would emit that subtree UNFLAGGED, while its unconditional
    //      `GetPrototypes()` pass emits it FLAGGED — two copies per prim.
    //   2. A subtree call that does NOT touch prototypes still re-emits them
    //      all (flagged) via `GetPrototypes()`; if the merge does not evict
    //      the previous copies it stacks a second FLAGGED set.
    let mut stage = StageService::open(fixture("instances/native_instances.usda"))
        .expect("open native instances");
    stage.project_topology().expect("full project topology");

    let proto_root = stage
        .topology()
        .iter()
        .filter_map(|r| stage.path(r.path))
        .find(|p| {
            p.starts_with("/__Prototype") && p.matches('/').count() == 1 // a `/__Prototype_N` root, not a descendant
        })
        .map(str::to_owned)
        .expect("instanced stage must expose a /__Prototype_N root");

    let assert_prototypes_clean = |stage: &StageService, phase: &str| {
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for r in stage.topology() {
            let Some(p) = stage.path(r.path) else {
                continue;
            };
            if p.starts_with("/__Prototype") {
                assert!(
                    r.is_prototype,
                    "{phase}: prototype prim {p} must stay flagged is_prototype",
                );
                *seen.entry(p).or_default() += 1;
            }
        }
        assert!(!seen.is_empty(), "{phase}: prototypes must remain present");
        for (path, count) in &seen {
            assert_eq!(
                *count, 1,
                "{phase}: prototype prim {path} appears {count}x (must be exactly once)",
            );
        }
    };

    // Case 1: subtree reprojection whose roots INCLUDE the prototype root.
    stage
        .project_topology_paths(&["/World".to_owned(), proto_root])
        .expect("subtree reproject incl. prototype root");
    assert_prototypes_clean(&stage, "with prototype root");

    // Case 2: subtree reprojection that does NOT touch prototypes.
    stage
        .project_topology_paths(&["/World".to_owned()])
        .expect("subtree reproject excl. prototype root");
    assert_prototypes_clean(&stage, "without prototype root");
}

#[test]
fn prototype_topology_records_are_flagged_is_prototype() {
    // The scene tree hides prototype subtrees by filtering on `is_prototype`
    // (idtx-studio-shell `build_tree_nodes`). That filter is only correct if
    // every prim inside a shared instancing prototype (`/__Prototype_N`) is
    // flagged — the root *and* its descendant Geom/Mesh prims.
    let mut stage = StageService::open(fixture("instances/native_instances.usda"))
        .expect("open native instances");
    stage.project_topology().expect("project topology");

    let protos: Vec<&TopologyRecord> = stage
        .topology()
        .iter()
        .filter(|r| {
            stage
                .path(r.path)
                .is_some_and(|p| p.starts_with("/__Prototype"))
        })
        .collect();
    assert!(
        !protos.is_empty(),
        "instanced stage must expose at least one /__Prototype_N subtree"
    );
    assert!(
        protos.iter().all(|r| r.is_prototype),
        "every prim in a prototype subtree (root + descendants) must be \
             flagged is_prototype so the scene tree can hide it"
    );
}

#[test]
fn native_instances_dedup_prototype_and_record_instances() {
    let mut stage = StageService::open(fixture("instances/native_instances.usda"))
        .expect("open native instances");
    // Native instances collapse to leaf prims; their instance-proxy geometry
    // (`/World/First/Geometry`) is no longer a tree node — the shared prototype
    // carries the geometry once.
    assert!(stage.prim_for_path("/World/First").is_ok());
    assert!(stage.prim_for_path("/World/Second").is_ok());

    let projection = stage.project_viewport().expect("project instances");

    // Prototype geometry is projected exactly ONCE, flagged prototype_only —
    // not once per instance. No instance emits a directly-drawn cube.
    assert_eq!(projection.cubes.len(), 1, "prototype cube projected once");
    assert!(
        projection.cubes.iter().all(|cube| cube.prototype_only),
        "the only projected cube must be prototype-only"
    );

    // Both instances are recorded, sharing a single prototype.
    assert_eq!(projection.native_instances.len(), 2);
    let prototype = projection.native_instances[0].prototype;
    assert!(
        projection
            .native_instances
            .iter()
            .all(|instance| instance.prototype == prototype),
        "both instances share one prototype"
    );
}

#[test]
fn rooted_point_instancer_changes_refresh_prototype_roles() {
    let mut stage = StageService::open(fixture("instances/point_instancer_variant.usda"))
        .expect("open PointInstancer variant fixture");
    let initial = stage.project_viewport().expect("initial projection");
    assert!(initial.cubes.iter().any(|record| record.prototype_only));
    assert!(initial.spheres.iter().any(|record| !record.prototype_only));

    let scatter = stage
        .prim_at_path("/World/Scatter")
        .expect("PointInstancer prim");
    stage
        .set_session_variant(scatter, "prototype", "Sphere")
        .expect("switch prototype relationship");
    let batch = stage
        .drain_changes()
        .expect("drain relationship change")
        .expect("relationship change batch");
    assert!(batch.changes.iter().any(|change| change.resync));

    let changed = stage
        .project_viewport_roots(&[scatter])
        .expect("refresh changed PointInstancer");
    assert_eq!(changed.point_instancers.len(), 1);
    let sphere = stage
        .prim_at_path("/World/Prototypes/Sphere")
        .expect("sphere prototype");
    assert_eq!(changed.point_instancers[0].prototypes, [sphere]);

    let prototypes = stage
        .prim_at_path("/World/Prototypes")
        .expect("prototype scope");
    let roles = stage
        .project_viewport_roots(&[prototypes])
        .expect("project prototype roles after rooted refresh");
    assert!(roles.cubes.iter().any(|record| !record.prototype_only));
    assert!(roles.spheres.iter().any(|record| record.prototype_only));
}

#[test]
fn root_scoped_reproject_skips_resident_prototype_geometry() {
    // Regression guard for the XR-preview whole-scene reprojection cost: a
    // root-scoped reproject (e.g. an XR controller pose write) must NOT
    // re-marshal every native-instancing prototype's geometry once it is
    // resident. The prototype meshes are projected once, retained by the
    // renderer across frames, and only re-read on a full/first projection or
    // after a topology resync.
    let mut stage = StageService::open(fixture("instances/native_instances.usda"))
        .expect("open native instances");

    // First full projection marshals — and makes resident — the shared
    // prototype geometry (one cube, prototype_only).
    let full = stage.project_viewport().expect("full projection");
    assert_eq!(full.cubes.len(), 1, "prototype cube projected once on full");
    assert!(
        full.cubes[0].prototype_only,
        "the projected cube is prototype-only"
    );

    // A root-scoped reproject of a non-prototype instance leaf must not emit
    // the already-resident prototype geometry again.
    let first = stage.prim_at_path("/World/First").expect("instance leaf");
    let scoped = stage
        .project_viewport_roots(&[first])
        .expect("root-scoped reproject");
    assert!(
        scoped.cubes.is_empty(),
        "resident prototype geometry must not be re-read on a root-scoped reproject"
    );

    // A subsequent full projection still re-marshals prototypes (residency
    // never hides geometry from a full projection).
    let full_again = stage.project_viewport().expect("second full projection");
    assert_eq!(full_again.cubes.len(), 1);
    assert!(full_again.cubes[0].prototype_only);
}

#[test]
fn preview_surface_constants_and_bound_sphere_are_typed_viewport_records() {
    let mut stage =
        StageService::open(fixture("materials/preview_surface.usda")).expect("open fixture");
    let projection = stage.project_viewport().expect("project viewport");
    assert_eq!(projection.spheres.len(), 2);
    let ball_prim = stage.prim_for_path("/World/Ball").expect("Ball prim");
    let sphere = projection
        .spheres
        .iter()
        .find(|s| s.prim == ball_prim)
        .expect("Ball sphere");
    assert_eq!(sphere.material_path.as_deref(), Some("/World/Looks/Blue"));
    let material = projection
        .materials
        .iter()
        .find(|material| material.path == "/World/Looks/Blue")
        .expect("bound material");
    assert!(
        material
            .base_color
            .iter()
            .zip([0.05, 0.2, 0.8])
            .all(|(actual, expected)| (actual - expected).abs() < 1.0e-6)
    );
    assert!((material.metallic - 0.2).abs() < 1.0e-6);
    assert!((material.roughness - 0.35).abs() < 1.0e-6);
}

#[test]
fn materialx_standard_surface_constants_lower_to_portable_material_ir() {
    let mut stage = StageService::open(fixture("materials/materialx_surface.usda"))
        .expect("open MaterialX fixture");
    let projection = stage.project_viewport().expect("project viewport");
    let material = projection
        .materials
        .iter()
        .find(|material| material.path == "/World/Material")
        .expect("MaterialX material");
    assert!(
        material
            .base_color
            .iter()
            .zip([0.8, 0.12, 0.04])
            .all(|(actual, expected)| (actual - expected).abs() < 1.0e-6)
    );
    assert!((material.metallic - 0.65).abs() < 1.0e-6);
    assert!((material.roughness - 0.25).abs() < 1.0e-6);
    assert!(material.diagnostics.is_empty());

    let material_prim = stage
        .prim_for_path("/World/Material")
        .expect("material prim");
    let graph = stage
        .material_graph(material_prim)
        .expect("MaterialX graph");
    assert_eq!(
        graph.nodes[0].identifier,
        "ND_standard_surface_surfaceshader"
    );
}

#[test]
fn materialx_standard_surface_transmission_color_projects_as_glass_tint() {
    let mut stage = StageService::open(fixture("materials/materialx_glass_transmission.usda"))
        .expect("open MaterialX glass fixture");
    let projection = stage.project_viewport().expect("project viewport");
    let material = projection
        .materials
        .iter()
        .find(|material| material.path == "/World/Looks/Glass")
        .expect("glass material");
    assert_eq!(material.opacity_mode, OpacityMode::Transparent);
    assert_eq!(material.surface.opacity_mode, OpacityMode::Transparent);

    // transmission_color is carried through as the Beer-Lambert glass tint.
    assert!(
        material
            .transmission_color
            .iter()
            .zip([0.1, 0.8, 0.2])
            .all(|(actual, expected)| (actual - expected).abs() < 1.0e-6),
        "transmission_color = {:?}",
        material.transmission_color
    );

    // The scalar `transmission = 0.9` still folds into opacity (independent
    // path), so the material is transparent (AlphaPolicy::Blend).
    assert!(
        material.opacity < 0.999,
        "scalar transmission should have reduced opacity, got {}",
        material.opacity
    );

    // A connected/authored transmission_color must not raise unsupported_input.
    assert!(
        material.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        material.diagnostics
    );
}

#[test]
fn preview_surface_ir_preserves_two_uvs_samplers_channels_and_color_intent() {
    let mut stage = StageService::open(fixture("materials/material_ir.usda"))
        .expect("open material IR fixture");
    let projection = stage.project_viewport().expect("project material IR");
    let material = projection
        .materials
        .iter()
        .find(|material| material.path == "/World/PreviewMaterial")
        .expect("Preview material");
    let surface = &material.surface;

    assert_eq!(surface.schema_version, MATERIAL_IR_SCHEMA_VERSION);
    assert_eq!(surface.model, SurfaceModel::UsdPreviewSurface);
    assert_eq!(surface.texture_inputs.len(), 6);
    assert_eq!(surface.uv_requirements.len(), 2);
    assert_eq!(surface.samplers.len(), 2);
    assert!(surface.feature_bits.contains(MaterialFeatures::TWO_UV_SETS));
    assert!(surface.source_nodes.len() <= MAX_MATERIAL_SOURCE_NODES);
    assert_ne!(surface.fingerprint.first, 0);
    assert_ne!(surface.fingerprint.second, 0);

    let texture = |semantic| {
        surface
            .texture_inputs
            .iter()
            .find(|texture| texture.semantic == semantic)
            .expect("texture semantic")
    };
    let assert_float4 = |actual: [f32; 4], expected: [f32; 4]| {
        assert!(
            actual
                .into_iter()
                .zip(expected)
                .all(|(actual, expected)| (actual - expected).abs() < 1.0e-6)
        );
    };
    let color = texture(TextureSemantic::BaseColor);
    assert_eq!(color.asset_path, "color.png");
    assert_eq!(color.interpretation, TextureInterpretation::SrgbColor);
    assert_float4(color.scale, [0.5, 0.75, 1.0, 1.0]);
    assert_float4(color.bias, [0.1, 0.2, 0.3, 0.0]);
    assert_float4(color.fallback, [0.25, 0.5, 0.75, 1.0]);
    assert_eq!(color.swizzle.component_count, 3);
    let color_uv = &surface.uv_requirements[usize::from(color.uv_requirement)];
    assert_eq!(color_uv.name, "st1");
    for (actual, expected) in color_uv
        .transform
        .into_iter()
        .zip([0.0, -3.0, 2.0, 0.0, 0.25, -0.5])
    {
        assert!((actual - expected).abs() < 1.0e-5);
    }
    let color_sampler = &surface.samplers[usize::from(color.sampler)];
    assert_eq!(color_sampler.wrap_s, TextureAddressMode::ClampToEdge);
    assert_eq!(color_sampler.wrap_t, TextureAddressMode::MirrorRepeat);
    assert_eq!(color_sampler.min_filter, TextureFilterMode::Nearest);
    assert_eq!(color_sampler.mag_filter, TextureFilterMode::Cubic);
    assert_eq!(color_sampler.mip_filter, TextureMipFilterMode::None);
    assert_eq!(color_sampler.max_anisotropy, 8);

    let metallic = texture(TextureSemantic::Metallic);
    assert_eq!(metallic.swizzle.channels[0], TextureChannel::Blue);
    assert_float4(metallic.scale, [0.5; 4]);
    assert_eq!(metallic.interpretation, TextureInterpretation::Data);
    let roughness = texture(TextureSemantic::Roughness);
    assert_eq!(roughness.swizzle.channels[0], TextureChannel::Green);
    assert_eq!(roughness.uv_requirement, metallic.uv_requirement);
    assert_ne!(roughness.uv_requirement, color.uv_requirement);
    assert_eq!(
        texture(TextureSemantic::Normal).interpretation,
        TextureInterpretation::Normal
    );
    assert_eq!(
        texture(TextureSemantic::Emissive).interpretation,
        TextureInterpretation::SceneLinearHdr
    );
    assert_eq!(
        texture(TextureSemantic::Opacity).swizzle.channels[0],
        TextureChannel::Alpha
    );
    assert!(material.uses_diffuse_texture_alpha);
    assert_eq!(material.metallic_texture_channel, 2);
    assert_eq!(material.roughness_texture_channel, 1);
    assert!(stage.metrics().viewport.bytes >= logical_material_bytes(material) as u64);
}

#[test]
fn connected_opacity_wins_over_authored_placeholder_scalar() {
    // Houdini/Karma export `float inputs:opacity = 0` alongside a live
    // `inputs:opacity.connect -> <diffuse texture>.outputs:a`. The connection must
    // win: the material stays opaque and samples the diffuse texture's alpha per
    // pixel. Treating the placeholder scalar 0 as authoritative would blend the
    // whole prim to invisible (the stairwell_lights_signage regression).
    let mut stage =
        StageService::open(fixture("materials/opacity_connected_to_diffuse_alpha.usda"))
            .expect("open opacity-connected fixture");
    let projection = stage.project_viewport().expect("project opacity fixture");
    let material = projection
        .materials
        .iter()
        .find(|material| material.path == "/World/Materials/OpacityConnected")
        .expect("bound material");

    // Connection wins → opacity default (1.0) retained, not the authored 0.
    assert_eq!(material.opacity, 1.0);
    assert!(material.uses_diffuse_texture_alpha);
    assert_eq!(material.surface.alpha_policy, AlphaPolicy::Opaque);
}

#[test]
fn opacity_modes_project_defaults_interfaces_diagnostics_and_masks() {
    let mut stage = StageService::open(fixture("materials/opacity_modes.usda")).unwrap();
    let projection = stage.project_viewport().unwrap();
    for (name, mode, policy) in [
        ("Default", OpacityMode::Transparent, AlphaPolicy::Blend),
        ("Transparent", OpacityMode::Transparent, AlphaPolicy::Blend),
        ("Presence", OpacityMode::Presence, AlphaPolicy::Blend),
        ("Interface", OpacityMode::Presence, AlphaPolicy::Blend),
        ("Invalid", OpacityMode::Transparent, AlphaPolicy::Blend),
        ("Mask", OpacityMode::Presence, AlphaPolicy::Mask),
    ] {
        let material = projection.materials.iter()
            .find(|m| m.path == format!("/World/{name}"))
            .unwrap();
        assert_eq!(material.opacity_mode, mode, "{name}");
        assert_eq!(material.surface.opacity_mode, mode, "{name}");
        assert_eq!(material.surface.alpha_policy, policy, "{name}");
        assert_eq!(material.surface.schema_version, MATERIAL_IR_SCHEMA_VERSION);
        assert_eq!(material.emissive, [0.15, 3.0, 1.95]);
        assert_eq!(
            material.surface.diagnostics.len(),
            usize::from(name == "Invalid"),
        );
        if name == "Invalid" {
            let diagnostic = &material.surface.diagnostics[0];
            assert_eq!(diagnostic.code, MaterialDiagnosticCode::UnsupportedInput);
            assert!(diagnostic.message.contains("unsupported"));
        }
    }
}

#[test]
fn opacity_mode_ffi_rejects_unknown_and_tampered_records() {
    let stage = StageService::open(fixture("materials/opacity_modes.usda")).unwrap();
    let raw = ffi::project_viewport(stage.handle_ref().unwrap(), 0.0, true, &[]).unwrap();
    let mut material = raw.materials.into_iter()
        .find(|m| m.path == "/World/Presence").unwrap();
    let factors = SurfaceFactors {
        base_color: [0.3, 0.4, 0.5],
        metallic: 1.0,
        roughness: 0.2,
        subsurface: 0.0,
        emissive: [0.15, 3.0, 1.95],
        opacity: 0.4,
        opacity_threshold: 0.0,
    };
    assert!(surface_ir(&material, factors).is_ok());
    material.opacity_mode = ffi::OpacityModeRaw::Transparent;
    assert!(surface_ir(&material, factors).unwrap_err()
        .to_string().contains("fingerprint"));
    material.opacity_mode = ffi::OpacityModeRaw { repr: 99 };
    assert!(surface_ir(&material, factors).unwrap_err()
        .to_string().contains("unknown opacity mode"));
    material.material_ir_version = 1;
    assert!(surface_ir(&material, factors).unwrap_err()
        .to_string().contains("schema version"));
}

#[test]
fn opacity_mode_reference_override_undo_save_and_runtime_package_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        fixture("materials/opacity_modes.usda"),
        dir.path().join("source.usda"),
    ).unwrap();
    let root = dir.path().join("root.usda");
    std::fs::write(&root, concat!(
        "#usda 1.0\n(defaultPrim = \"World\")\n",
        "def Xform \"World\" (prepend references = @source.usda@</World>) {}\n",
    )).unwrap();
    let mut stage = StageService::open(&root).unwrap();
    stage.project_topology().unwrap();
    let shader = stage.prim_at_path("/World/Transparent/Shader").unwrap();
    let projected = |stage: &mut StageService| {
        stage.project_viewport().unwrap().materials.into_iter()
            .find(|m| m.path == "/World/Transparent").unwrap()
    };
    let original = projected(&mut stage);
    let before = stage.root_property_text(shader, "inputs:opacityMode").unwrap();
    assert!(before.is_empty(), "mode originally comes from the reference");
    stage.author_targeted_property(
        shader,
        "inputs:opacityMode",
        &PropertyValue::Token("presence".into()),
        &EditTarget::Root,
    ).unwrap();
    let changes = stage.drain_changes().unwrap().unwrap();
    assert!(changes.changes.iter()
        .any(|c| c.domains.contains(DirtyDomains::MATERIAL)));
    let presence = projected(&mut stage);
    assert_eq!(presence.surface.opacity_mode, OpacityMode::Presence);
    assert_eq!(original.opacity, presence.opacity);
    assert_eq!(original.emissive, presence.emissive);
    assert_ne!(original.surface.fingerprint, presence.surface.fingerprint);
    let after = stage.root_property_text(shader, "inputs:opacityMode").unwrap();
    stage.replace_root_property_text(shader, "inputs:opacityMode", &before)
        .unwrap();
    stage.drain_changes().unwrap();
    assert_eq!(
        projected(&mut stage).surface.fingerprint,
        original.surface.fingerprint,
    );
    stage.replace_root_property_text(shader, "inputs:opacityMode", &after)
        .unwrap();
    stage.drain_changes().unwrap();
    assert_eq!(
        projected(&mut stage).surface.fingerprint,
        presence.surface.fingerprint,
    );
    stage.save_stage(&root).unwrap();
    drop(stage);
    let mut reopened = StageService::open(&root).unwrap();
    assert_eq!(projected(&mut reopened).surface, presence.surface);
    let cooked = dir.path().join("runtime.usdz");
    package_stage(&root, &cooked, PackagingPurpose::Runtime).unwrap();
    let mut packaged = StageService::open(&cooked).unwrap();
    assert_eq!(projected(&mut packaged).surface, presence.surface);
}

#[test]
fn opacity_mode_presence_enables_connected_alpha_blending() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alpha.usda");
    let source = std::fs::read_to_string(fixture(
        "materials/opacity_connected_to_diffuse_alpha.usda",
    )).unwrap();
    std::fs::write(&path, source.replace(
        "float inputs:opacity = 0",
        "token inputs:opacityMode = \"presence\"\n            float inputs:opacity = 0",
    )).unwrap();
    let mut stage = StageService::open(path).unwrap();
    let material = stage.project_viewport().unwrap().materials.into_iter()
        .find(|m| m.path == "/World/Materials/OpacityConnected")
        .unwrap();
    assert_eq!(material.opacity_mode, OpacityMode::Presence);
    assert_eq!(material.opacity, 1.0, "connection overrides authored scalar");
    assert!(material.uses_diffuse_texture_alpha);
    assert_eq!(material.surface.alpha_policy, AlphaPolicy::Blend);
}

#[test]
fn materialx_scalar_extract_and_convert_chain_retains_provenance() {
    let mut stage = StageService::open(fixture("materials/material_ir.usda"))
        .expect("open material IR fixture");
    let projection = stage.project_viewport().expect("project material IR");
    let surface = &projection
        .materials
        .iter()
        .find(|material| material.path == "/World/MaterialXMaterial")
        .expect("MaterialX material")
        .surface;
    assert_eq!(surface.model, SurfaceModel::MaterialXStandardSurface);
    let metallic = surface
        .texture_inputs
        .iter()
        .find(|texture| texture.semantic == TextureSemantic::Metallic)
        .expect("connected metalness");
    assert_eq!(metallic.swizzle.channels[0], TextureChannel::Green);
    assert_eq!(metallic.interpretation, TextureInterpretation::Data);
    let identifiers = metallic
        .source_node_indices
        .iter()
        .map(|index| {
            surface.source_nodes[usize::from(*index)]
                .identifier
                .as_str()
        })
        .collect::<Vec<_>>();
    assert!(identifiers.contains(&"ND_convert_float"));
    assert!(identifiers.contains(&"ND_extract_color4"));
    assert!(identifiers.contains(&"ND_image_color4"));
}

#[test]
fn unsupported_material_cycles_and_complex_math_are_deterministic() {
    let mut stage = StageService::open(fixture("materials/material_ir.usda"))
        .expect("open material IR fixture");
    let first = stage.project_viewport().expect("first material projection");
    let second = stage
        .project_viewport()
        .expect("repeat material projection");
    let first = first
        .materials
        .iter()
        .find(|material| material.path == "/World/UnsupportedMaterial")
        .expect("unsupported material");
    let second = second
        .materials
        .iter()
        .find(|material| material.path == "/World/UnsupportedMaterial")
        .expect("unsupported material");
    assert_eq!(first.surface.fingerprint, second.surface.fingerprint);
    assert_eq!(first.surface.diagnostics, second.surface.diagnostics);
    assert!(first.surface.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == MaterialDiagnosticCode::UnsupportedNode
            && diagnostic.message.contains("ND_add_float")
    }));
    assert!(first.surface.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == MaterialDiagnosticCode::GraphCycle
            && diagnostic.message.contains("CycleA")
    }));
    assert!(first.surface.texture_inputs.is_empty());
}

#[test]
fn common_revolved_gprims_are_projected_with_axis_and_dimensions() {
    let fixture = fixture("scene/revolved_gprims.usda");
    let mut stage = StageService::open(fixture).expect("open gprim fixture");
    let projection = stage.project_viewport().expect("project gprims");
    assert_eq!(projection.extended_gprims.len(), 3);
    assert!(projection.extended_gprims.iter().any(|record| {
        record.kind == ExtendedGprimKind::Capsule && record.axis == GprimAxis::Z
    }));
    assert!(projection.extended_gprims.iter().any(|record| {
        record.kind == ExtendedGprimKind::Cone && (record.height - 2.0).abs() < 1.0e-6
    }));
    assert!(projection.diagnostics.iter().all(|diagnostic| {
        !diagnostic.contains("unsupported viewport gprim Cone")
            && !diagnostic.contains("unsupported viewport gprim Cylinder")
            && !diagnostic.contains("unsupported viewport gprim Capsule")
    }));
}

#[test]
fn plane_prims_are_projected_with_axis_width_length_and_material() {
    let fixture = fixture("scene/plane_basic.usda");
    let mut stage = StageService::open(fixture).expect("open plane fixture");
    let projection = stage.project_viewport().expect("project planes");

    assert_eq!(projection.planes.len(), 3, "three planes projected");

    // axis X → width 2, length 4.
    assert!(
        projection.planes.iter().any(|p| {
            p.axis == GprimAxis::X
                && (p.width - 2.0).abs() < 1.0e-6
                && (p.length - 4.0).abs() < 1.0e-6
        }),
        "expected an X-axis plane with width 2 / length 4"
    );
    // axis Y → authored display colour.
    assert!(
        projection
            .planes
            .iter()
            .any(|p| p.axis == GprimAxis::Y && !p.display_colors.is_empty()),
        "expected a Y-axis plane carrying a displayColor"
    );
    // axis Z → bound material, double-sided (plane default).
    assert!(
        projection.planes.iter().any(|p| {
            p.axis == GprimAxis::Z
                && p.material_path.as_deref() == Some("/World/Looks/Red")
                && p.double_sided
        }),
        "expected a Z-axis plane bound to /World/Looks/Red"
    );
    // Plane is no longer reported as an unsupported viewport gprim.
    assert!(
        projection
            .diagnostics
            .iter()
            .all(|d| !d.contains("unsupported viewport gprim Plane")),
        "Plane must not be flagged unsupported"
    );
}

#[test]
fn unsupported_skinning_and_surface_nodes_report_deterministic_fallbacks() {
    let mut stage = StageService::open(fixture("scene/unsupported_features.usda"))
        .expect("open unsupported fixture");
    let projection = stage.project_viewport().expect("project fallbacks");
    assert!(projection.diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("UsdSkel") && diagnostic.contains("rest-pose fallback")
    }));
    let material = projection
        .materials
        .iter()
        .find(|material| material.path == "/World/UnknownMaterial")
        .expect("fallback material");
    assert!(material.diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("UnsupportedSurface")
            && diagnostic.contains("deterministic gray fallback")
    }));
}

#[test]
fn preview_surface_texture_and_common_st0_uvs_are_projected() {
    let fixture = fixture("materials/TextureCoordinateTest.usda");
    support::usd_fixture("materials/TextureCoordinateTemplate.png");
    let mut stage = StageService::open(fixture).expect("open texture corpus fixture");
    let projection = stage.project_viewport().expect("project viewport");
    let textured = projection
        .materials
        .iter()
        .filter_map(|material| material.diffuse_texture.as_deref())
        .collect::<Vec<_>>();
    assert!(!textured.is_empty(), "bound texture should be resolved");
    assert!(textured.iter().all(|path| Path::new(path).is_file()));
    assert!(
        projection
            .meshes
            .iter()
            .any(|mesh| !mesh.texcoords.is_empty()),
        "st0 vertex primvars should reach the typed projection"
    );
    assert!(projection.meshes.iter().all(|mesh| {
        mesh.texcoords.is_empty() || mesh.texcoords.len() == mesh.face_vertex_indices.len()
    }));
}

#[test]
fn mesh_orientation_is_preserved_in_the_typed_projection() {
    let mut stage = StageService::open(fixture("scene/mesh_orientation.usda"))
        .expect("open orientation fixture");
    let projection = stage
        .project_viewport()
        .expect("project orientation fixture");
    assert_eq!(projection.meshes.len(), 2);
    assert_eq!(
        projection
            .meshes
            .iter()
            .filter(|mesh| mesh.left_handed)
            .count(),
        1
    );
}

#[test]
fn preview_surface_selects_the_uv_primvar_named_by_its_texture_reader() {
    let mut stage = StageService::open(fixture("materials/multiple_uv_sets.usda"))
        .expect("open multiple UV fixture");
    let projection = stage.project_viewport().expect("project UV sets");
    let mesh = projection.meshes.first().expect("mesh");
    assert!((mesh.texcoords[0][0] - 0.2).abs() < 1.0e-6);
    assert!((mesh.texcoords[0][1] - 0.3).abs() < 1.0e-6);
    let material = projection.materials.first().expect("material");
    assert_eq!(material.diffuse_uv_set.as_deref(), Some("st1"));
    assert!(material.uses_diffuse_texture_alpha);
    for (actual, expected) in material
        .diffuse_uv_transform
        .into_iter()
        .zip([0.0, -3.0, 2.0, 0.0, 0.25, -0.5])
    {
        assert!((actual - expected).abs() < 1.0e-5);
    }
}

#[test]
fn topology_projects_packed_arc_and_exact_root_layer_specifiers() {
    let stage =
        StageService::open(fixture("skinning/packed_host.usda")).expect("open packed fixture");
    let row = |path: &str| {
        stage
            .topology()
            .iter()
            .find(|row| stage.path(row.path) == Some(path))
            .expect("topology row")
    };
    let boundary = row("/World/Packed");
    assert_eq!(
        boundary.packed_arc.as_ref().map(|arc| arc.kind),
        Some(PackedArcKind::Reference)
    );
    assert_eq!(
        boundary
            .packed_arc
            .as_ref()
            .map(|arc| arc.asset_path.as_str()),
        Some("packed_asset.usda")
    );
    assert_eq!(
        row("/World/Packed/ReferencedBody").local_specifier,
        Some(PrimSpecifier::Over)
    );
    assert_eq!(
        row("/World/Packed/LocalDef").local_specifier,
        Some(PrimSpecifier::Def)
    );
    assert_eq!(row("/World/Packed/ReferencedDetail").local_specifier, None);
}

#[test]
fn rooted_topology_projection_emits_leaf_upserts_and_deletion_tombstones() {
    let isolated = isolated_fixture("rooted-topology");
    let mut stage = StageService::open(&isolated).expect("open fixture");
    let records_before = stage.metrics().topology.records;
    stage
        .apply_structural_command(&StructuralCommand::Define {
            path: "/World/Leaf".to_owned(),
            type_name: "Cube".to_owned(),
        })
        .expect("define leaf");
    stage.drain_changes().expect("drain define").expect("batch");
    let delta = stage
        .project_topology_paths(&["/World/Leaf".to_owned()])
        .expect("rooted projection");
    assert_eq!(delta.upserts.len(), 1);
    assert!(delta.removed.is_empty());
    assert_eq!(stage.metrics().topology.records - records_before, 1);
    let leaf = stage.prim_at_path("/World/Leaf").expect("leaf");
    stage
        .apply_structural_command(&StructuralCommand::Delete { prim: leaf })
        .expect("delete leaf");
    stage.drain_changes().expect("drain delete").expect("batch");
    let delta = stage
        .project_topology_paths(&["/World/Leaf".to_owned()])
        .expect("deletion projection");
    assert!(delta.upserts.is_empty());
    assert_eq!(delta.removed, HashSet::from([leaf]));
    assert!(!stage.is_live_prim(leaf));
    std::fs::remove_file(isolated).expect("remove fixture");
}

#[test]
fn set_active_false_prunes_subtree_then_true_restores_it() {
    let isolated = isolated_fixture("set-active");
    let mut stage = StageService::open(&isolated).expect("open fixture");
    // The fixture defines /World/Group (a root-layer def) with a child Cube.
    let group = stage.prim_at_path("/World/Group").expect("group");
    let cube = stage.prim_at_path("/World/Group/Cube").expect("cube");

    // Soft delete: author active=false on the group.
    stage
        .apply_structural_command(&StructuralCommand::SetActive {
            path: "/World/Group".to_owned(),
            active: false,
        })
        .expect("deactivate group");
    stage
        .drain_changes()
        .expect("drain deactivate")
        .expect("batch");
    // A full reprojection traverses only *active* prims, so the whole inactive
    // subtree drops out of the topology store.
    stage
        .project_topology()
        .expect("reproject after deactivate");
    assert!(
        stage.prim_at_path("/World/Group").is_none(),
        "the deactivated group must be pruned from the topology projection"
    );
    assert!(
        stage.prim_at_path("/World/Group/Cube").is_none(),
        "the deactivated group's descendants must be pruned too"
    );

    // Restore: author active=true. Addressed by path (the prim has no live
    // PrimId while inactive), and the now-active root re-emits its subtree.
    stage
        .apply_structural_command(&StructuralCommand::SetActive {
            path: "/World/Group".to_owned(),
            active: true,
        })
        .expect("reactivate group");
    stage
        .drain_changes()
        .expect("drain reactivate")
        .expect("batch");
    let delta = stage
        .project_topology_paths(&["/World/Group".to_owned()])
        .expect("rooted reprojection after reactivate");
    let upsert_paths: Vec<&str> = delta
        .upserts
        .iter()
        .filter_map(|r| stage.path(r.path))
        .collect();
    assert!(
        upsert_paths.contains(&"/World/Group"),
        "the reactivated group must be re-emitted as an upsert, got {upsert_paths:?}"
    );
    assert!(
        upsert_paths.contains(&"/World/Group/Cube"),
        "the reactivated group's descendants must be re-emitted, got {upsert_paths:?}"
    );
    // Stable prim identity survives the deactivate/reactivate cycle (identity
    // is keyed by path, so selection and bindings stored by PrimId recover).
    assert_eq!(
        stage.prim_at_path("/World/Group"),
        Some(group),
        "the restored group must keep its original PrimId"
    );
    assert_eq!(stage.prim_at_path("/World/Group/Cube"), Some(cube));
    std::fs::remove_file(isolated).expect("remove fixture");
}

#[test]
fn topology_churn_keeps_identity_retention_bounded_without_reusing_ids() {
    let isolated = isolated_fixture("bounded-identity-retention");
    let mut stage = StageService::open(&isolated).expect("open fixture");
    stage.set_identity_retention_policy(IdentityRetentionPolicy::new(3, u64::MAX));
    let baseline_live = stage.identity_retention_stats().live_prims;
    let mut first_identity = None;

    for index in 0..12 {
        let path = format!("/World/Churn{index}");
        stage
            .apply_structural_command(&StructuralCommand::Define {
                path: path.clone(),
                type_name: "Xform".to_owned(),
            })
            .expect("define churn prim");
        stage.drain_changes().expect("drain define").expect("batch");
        stage
            .project_topology_paths(std::slice::from_ref(&path))
            .expect("project defined prim");
        let prim = stage.prim_at_path(&path).expect("defined prim identity");
        let path_id = stage
            .topology()
            .iter()
            .find(|record| record.prim == prim)
            .expect("defined topology record")
            .path;
        first_identity.get_or_insert((prim, path_id));

        stage
            .apply_structural_command(&StructuralCommand::Delete { prim })
            .expect("delete churn prim");
        stage.drain_changes().expect("drain delete").expect("batch");
        stage
            .project_topology_paths(std::slice::from_ref(&path))
            .expect("project deleted prim");

        let stats = stage.identity_retention_stats();
        assert_eq!(stats.live_prims, baseline_live);
        assert!(stats.retained_tombstones <= 3);
        assert_eq!(
            stats.interned_paths,
            stats.live_prims + stats.retained_tombstones
        );
    }

    let (first_prim, first_path) = first_identity.expect("first identity");
    assert_eq!(stage.path(first_path), None);
    assert_eq!(stage.path_for(first_prim), None);

    let path = "/World/Churn0".to_owned();
    stage
        .apply_structural_command(&StructuralCommand::Define {
            path: path.clone(),
            type_name: "Xform".to_owned(),
        })
        .expect("redefine evicted path");
    stage
        .drain_changes()
        .expect("drain redefine")
        .expect("batch");
    stage
        .project_topology_paths(std::slice::from_ref(&path))
        .expect("project redefined path");
    let replacement = stage.prim_at_path(&path).expect("replacement identity");
    let replacement_path = stage
        .topology()
        .iter()
        .find(|record| record.prim == replacement)
        .expect("replacement topology record")
        .path;
    assert!(replacement.slot() > first_prim.slot());
    assert!(replacement_path.get() > first_path.get());

    std::fs::remove_file(isolated).expect("remove fixture");
}

#[test]
fn a_reintroduced_retained_path_recovers_its_stage_scoped_identity() {
    let isolated = isolated_fixture("retained-identity-reuse");
    let mut stage = StageService::open(&isolated).expect("open fixture");
    stage.set_identity_retention_policy(IdentityRetentionPolicy::new(8, 8));
    let path = "/World/Reintroduced".to_owned();
    stage
        .apply_structural_command(&StructuralCommand::Define {
            path: path.clone(),
            type_name: "Xform".to_owned(),
        })
        .expect("define prim");
    stage.drain_changes().expect("drain define").expect("batch");
    stage
        .project_topology_paths(std::slice::from_ref(&path))
        .expect("project define");
    let original = stage.prim_at_path(&path).expect("original prim");
    let original_path = stage
        .topology()
        .iter()
        .find(|record| record.prim == original)
        .expect("original topology record")
        .path;

    stage
        .apply_structural_command(&StructuralCommand::Delete { prim: original })
        .expect("delete prim");
    stage.drain_changes().expect("drain delete").expect("batch");
    stage
        .project_topology_paths(std::slice::from_ref(&path))
        .expect("project delete");
    assert_eq!(stage.path(original_path), Some(path.as_str()));
    assert_eq!(stage.path_for(original), Some(path.as_str()));

    stage
        .apply_structural_command(&StructuralCommand::Define {
            path: path.clone(),
            type_name: "Xform".to_owned(),
        })
        .expect("redefine prim");
    stage
        .drain_changes()
        .expect("drain redefine")
        .expect("batch");
    stage
        .project_topology_paths(std::slice::from_ref(&path))
        .expect("project redefine");
    assert_eq!(stage.prim_at_path(&path), Some(original));
    assert_eq!(stage.identity_retention_stats().retained_tombstones, 0);

    std::fs::remove_file(isolated).expect("remove fixture");
}

/// Workspace-root compute-graph fixture (not the crate-local parse fixtures).
fn compute_fixture() -> PathBuf {
    fixture("compute/compute_graph_chain.usda")
}

/// The backward walk from a connected consumer prim reaches every upstream
/// compute node and terminates at the datasource root, classifying roles by
/// type name (works without the openexec plugin registered).
#[test]
fn compute_graph_walks_backward_to_datasource_root() {
    let mut stage = StageService::open(compute_fixture()).expect("open compute fixture");
    let cube = stage.prim_at_path("/World/Cube").expect("cube prim");
    let graph = stage.compute_graph(cube).expect("compute graph");

    assert_eq!(graph.root_path.as_deref(), Some("/World/Cube"));
    assert_eq!(graph.nodes.len(), 3, "cube + extract + src");
    assert!(graph.diagnostics.is_empty());

    let node = |path: &str| {
        graph
            .nodes
            .iter()
            .find(|n| n.path == path)
            .unwrap_or_else(|| panic!("missing node {path}"))
    };

    let cube_node = node("/World/Cube");
    assert_eq!(cube_node.role, ComputeNodeRole::Consumer);
    // Only the connected `size` becomes an input socket — visibility /
    // extent / xformOpOrder (unconnected, not `inputs:*`) are excluded.
    assert_eq!(cube_node.inputs.len(), 1);
    assert_eq!(cube_node.inputs[0].name, "size");
    assert_eq!(
        cube_node.inputs[0].connections,
        vec!["/World/Data/Extract.outputs:jsonValue:double".to_owned()]
    );

    let extract = node("/World/Data/Extract");
    assert_eq!(extract.role, ComputeNodeRole::Compute);
    let json_in = extract
        .inputs
        .iter()
        .find(|s| s.name == "inputs:jsonData")
        .expect("jsonData input");
    assert_eq!(
        json_in.connections,
        vec!["/World/Data/Src.outputs:data".to_owned()]
    );
    assert!(
        extract
            .outputs
            .iter()
            .any(|s| s.name == "outputs:jsonValue:double")
    );

    let src = node("/World/Data/Src");
    assert_eq!(src.role, ComputeNodeRole::Datasource);
    // The datasource is a graph root: no connected inputs (walk terminates).
    assert!(src.inputs.is_empty());
    assert!(src.outputs.iter().any(|s| s.name == "outputs:data"));
}

/// A prim with no connected attribute yields an empty graph (`root_path`
/// `None`) — the Compute Graph tab shows its empty-state hint.
#[test]
fn compute_graph_empty_for_unconnected_prim() {
    let mut stage = StageService::open(compute_fixture()).expect("open compute fixture");
    let plain = stage
        .prim_at_path("/World/Plain")
        .expect("plain sphere prim");
    let graph = stage.compute_graph(plain).expect("compute graph");
    assert_eq!(graph.root_path, None);
    assert!(graph.nodes.is_empty());
}

#[test]
fn material_graph_uses_computed_inherited_binding() {
    let mut stage =
        StageService::open(fixture("materials/preview_surface.usda")).expect("open fixture");
    let sphere = stage
        .prim_at_path("/World/InheritedBinding")
        .expect("sphere");
    let graph = stage.material_graph(sphere).expect("material graph");
    assert_eq!(graph.material_path.as_deref(), Some("/World/Looks/Blue"));
    assert_eq!(graph.nodes.len(), 1);
    assert_eq!(graph.nodes[0].identifier, "UsdPreviewSurface");
    assert!(graph.diagnostics.is_empty());
}

#[test]
fn material_graph_preserves_upstream_texture_connections() {
    let fixture = fixture("materials/TextureCoordinateTest.usda");
    let mut stage = StageService::open(fixture).expect("open texture fixture");
    let material = stage
        .prim_at_path("/Asset/Materials/UsdPreviewSurface/Materials/BottomLeftMat")
        .expect("material");
    let graph = stage.material_graph(material).expect("material graph");
    assert!(graph.nodes.len() >= 3);
    let surface = graph
        .nodes
        .iter()
        .find(|node| node.identifier == "UsdPreviewSurface")
        .expect("surface node");
    assert!(surface.inputs.iter().any(|input| {
        input.name == "diffuseColor"
            && input
                .connections
                .iter()
                .any(|connection| connection.contains("TextureMapper"))
    }));
}

// ── Uber-bridge integration tests ──────────────────────────────────────

fn uber_fixture(name: &str) -> PathBuf {
    fixture(&format!("scene/{name}"))
}

#[test]
fn vec3f_bulk_copy_matches_element_wise() {
    let src: Vec<[f32; 3]> = vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [0.0, -1.0, 0.5]];
    let mut dst = vec![glam::Vec3::ZERO; src.len()];
    vec3f_bulk_copy(&src, &mut dst);
    for (got, expected) in dst.iter().zip(src.iter()) {
        assert_eq!(got.x, expected[0]);
        assert_eq!(got.y, expected[1]);
        assert_eq!(got.z, expected[2]);
    }
}

// ── Layer stack integration tests ──────────────────────────────────────
//
// Exercise `layer_stack()` / `reload_layer()` against a multi-sublayer
// fixture. Require the OpenUSD runtime (DYLD_LIBRARY_PATH / LD_LIBRARY_PATH)
// and the required fixtures under tests/fixtures/usd/layering/.

fn layerstack_fixture(name: &str) -> PathBuf {
    fixture(&format!("layering/{name}"))
}

/// Copy the reference-scene fixture and its (relatively-referenced) asset
/// into a fresh temp directory so authoring tests never share USD's
/// process-wide layer cache with each other or with the on-disk fixture.
/// Returns the isolated root path; missing fixtures panic in the shared helper.

fn isolated_ref_scene() -> (PathBuf, PathBuf) {
    let root = layerstack_fixture("layerstack_ref_scene.usda");
    let asset = layerstack_fixture("layerstack_widget_asset.usda");
    let dir = std::env::temp_dir().join(format!(
        "idtx-opinion-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&dir).expect("create isolated fixture dir");
    let root_copy = dir.join("layerstack_ref_scene.usda");
    std::fs::copy(&root, &root_copy).expect("copy ref scene");
    std::fs::copy(&asset, dir.join("layerstack_widget_asset.usda")).expect("copy asset");
    (root_copy, dir)
}

#[test]
fn layer_stack_returns_session_root_and_sublayers_strongest_first() {
    let f = layerstack_fixture("layerstack_root.usda");
    let mut stage = StageService::open(&f).expect("open layerstack fixture");
    stage.project_topology().expect("topology");

    let layers = stage.layer_stack().expect("layer_stack");
    let ids: Vec<&str> = layers.iter().map(|l| l.identifier.as_str()).collect();

    // session + root + 2 sublayers.
    assert!(layers.len() >= 4, "expected >= 4 layers, got {ids:?}");

    // Session layer is strongest (first) and anonymous.
    assert_eq!(layers[0].arc, LayerArcKind::Session, "layers: {ids:?}");
    assert!(layers[0].is_anonymous);
    assert_eq!(layers[0].depth, 0);

    // Root layer follows the session layer.
    let root = &layers[1];
    assert_eq!(root.arc, LayerArcKind::Root);
    assert!(
        root.identifier.ends_with("layerstack_root.usda"),
        "root id: {}",
        root.identifier
    );
    assert_eq!(root.depth, 0);
    assert!(root.delta_count >= 1, "root must contribute prim specs");

    // Sublayers follow the root, strong before weak (declaration order = strength).
    let strong_idx = layers
        .iter()
        .position(|l| l.identifier.ends_with("layerstack_strong.usda"))
        .expect("strong sublayer present");
    let weak_idx = layers
        .iter()
        .position(|l| l.identifier.ends_with("layerstack_weak.usda"))
        .expect("weak sublayer present");
    assert!(
        strong_idx < weak_idx,
        "strong sublayer must precede weak: {ids:?}"
    );
    assert_eq!(layers[strong_idx].arc, LayerArcKind::SubLayer);
    assert_eq!(layers[weak_idx].arc, LayerArcKind::SubLayer);
    assert_eq!(layers[strong_idx].depth, 1);

    // The strong sublayer has an identity offset; the weak one carries the
    // authored `(offset = 24; scale = 2)` from the root's subLayers list.
    assert_eq!(layers[strong_idx].offset, 0.0);
    assert_eq!(layers[strong_idx].scale, 1.0);
    assert_eq!(layers[weak_idx].offset, 24.0);
    assert_eq!(layers[weak_idx].scale, 2.0);
}

#[test]
fn reload_layer_succeeds_for_root() {
    let f = layerstack_fixture("layerstack_root.usda");
    let mut stage = StageService::open(&f).expect("open layerstack fixture");
    stage.project_topology().expect("topology");
    let layers = stage.layer_stack().expect("layer_stack");
    let root = layers
        .iter()
        .find(|l| l.arc == LayerArcKind::Root)
        .expect("root layer");
    // Reloading a clean, on-disk layer must succeed without error.
    stage
        .reload_layer(&root.identifier)
        .expect("reload root layer");
}

#[test]
fn layer_stack_surfaces_reference_layers_with_mapping() {
    let f = layerstack_fixture("layerstack_ref_scene.usda");
    let mut stage = StageService::open(&f).expect("open reference fixture");
    stage.project_topology().expect("topology");

    let layers = stage.layer_stack().expect("layer_stack");
    let refs: Vec<&LayerRecord> = layers
        .iter()
        .filter(|l| l.arc == LayerArcKind::Reference)
        .collect();
    assert_eq!(
        refs.len(),
        1,
        "expected exactly one referenced layer, got {:?}",
        layers.iter().map(|l| &l.identifier).collect::<Vec<_>>()
    );
    let widget = refs[0];
    assert!(
        widget.identifier.ends_with("layerstack_widget_asset.usda"),
        "ref id: {}",
        widget.identifier
    );
    assert_eq!(widget.arc_source_path.as_deref(), Some("/World/Widget"));
    assert_eq!(widget.arc_target_path.as_deref(), Some("/Widget"));
    assert!(widget.has_specs, "referenced asset must contribute specs");
    assert!(widget.is_loaded, "references are always loaded");
    assert!(widget.delta_count >= 1);
}

/// "Create Opinion": author an empty root-layer `over` for a display-only
/// prim (composed only through a reference arc), then "Remove Opinion" to
/// revert it. Validates the Risk in the plan — `OverridePrim` must emit a
/// TOPOLOGY resync so the cached provenance flips `is_locally_authored`.

#[test]
fn create_and_remove_opinion_round_trips_display_only_prim() {
    let (f, dir) = isolated_ref_scene();
    let mut stage = StageService::open(&f).expect("open reference fixture");
    stage.project_topology().expect("topology");

    // `/World/Widget` carries the reference arc; its child `Body` is composed
    // purely from the referenced asset, so it is display-only (no root spec).
    let body = "/World/Widget/Body";

    let root_id = {
        let layers = stage.layer_stack().expect("layer_stack");
        layers
            .iter()
            .find(|l| l.arc == LayerArcKind::Root)
            .expect("root layer")
            .identifier
            .clone()
    };

    let provenance = |stage: &StageService, path: &str| {
        stage
            .topology()
            .iter()
            .find(|row| stage.path(row.path) == Some(path))
            .map(|row| (row.local_specifier, row.local_layer_identifier.clone()))
            .expect("topology row")
    };

    // Before: display-only — no opinion in any local layer.
    let (spec, layer) = provenance(&stage, body);
    assert_eq!(spec, None, "child must be display-only before create-over");
    assert_eq!(layer, None);

    // Author the empty over into the root layer.
    stage
        .apply_structural_command(&StructuralCommand::CreateOverride {
            path: body.to_owned(),
        })
        .expect("create over");

    // Risk resolution: authoring the over specifier is a prim *metadata*
    // change — USD emits an info-only `ObjectsChanged` notice (no resync,
    // no TOPOLOGY domain), so the resync-driven tick() topology path does
    // NOT fire. This is exactly why `StudioSession::create_opinion`
    // reprojects the affected path explicitly; without it the cached
    // provenance (and `is_locally_authored`) would never refresh.
    let batch = stage
        .drain_changes()
        .expect("drain create-over")
        .expect("create-over must emit a change notice");
    let body_prim = stage.prim_at_path(body).expect("composed prim identity");
    let record = batch
        .changes
        .iter()
        .find(|c| c.prim == body_prim)
        .expect("change record for the overridden prim");
    assert!(
        !record.resync,
        "an inert over is an info-only change, not a resync",
    );
    assert!(
        !record.domains.contains(DirtyDomains::TOPOLOGY),
        "an inert over must not report a TOPOLOGY change; got {:?}",
        record.domains,
    );
    assert!(
        record.authored_in_root_layer,
        "the opinion is authored in the root layer",
    );

    // The explicit reprojection (mirroring the session wrapper) is what
    // refreshes provenance.
    stage
        .project_topology_paths(&[body.to_owned()])
        .expect("reproject after create-over");

    // After reprojection the child carries a root-layer `over`.
    let (spec, layer) = provenance(&stage, body);
    assert_eq!(spec, Some(PrimSpecifier::Over), "over must be authored");
    assert_eq!(
        layer.as_deref(),
        Some(root_id.as_str()),
        "opinion must live in the root layer",
    );

    // Idempotent: a second create-over on the same path is a no-op.
    stage
        .apply_structural_command(&StructuralCommand::CreateOverride {
            path: body.to_owned(),
        })
        .expect("idempotent create-over");
    let _ = stage.drain_changes().expect("drain idempotent create-over");
    stage
        .project_topology_paths(&[body.to_owned()])
        .expect("reproject after idempotent create-over");
    assert_eq!(
        provenance(&stage, body).0,
        Some(PrimSpecifier::Over),
        "idempotent create-over must leave exactly one over spec",
    );

    // Remove Opinion: the empty over is stripped and the prim reverts.
    stage
        .apply_structural_command(&StructuralCommand::RemoveOverride {
            path: body.to_owned(),
        })
        .expect("remove over");
    stage.drain_changes().expect("drain remove-over");
    stage
        .project_topology_paths(&[body.to_owned()])
        .expect("reproject after remove-over");
    let (spec, layer) = provenance(&stage, body);
    assert_eq!(spec, None, "over must be gone after remove-over");
    assert_eq!(layer, None);

    drop(stage);
    std::fs::remove_dir_all(dir).expect("cleanup isolated fixture");
}

/// "Remove Opinion" must never destroy authored data: once the over carries
/// a property spec, `remove-over` refuses (and the over survives).

#[test]
fn remove_opinion_refuses_an_override_carrying_authored_data() {
    let (f, dir) = isolated_ref_scene();
    let mut stage = StageService::open(&f).expect("open reference fixture");
    stage.project_topology().expect("topology");
    let body = "/World/Widget/Body";

    stage
        .apply_structural_command(&StructuralCommand::CreateOverride {
            path: body.to_owned(),
        })
        .expect("create over");
    stage.drain_changes().expect("drain create-over");

    // Author a value onto the over so it is no longer inert.
    let body_prim = stage.prim_at_path(body).expect("composed prim identity");
    stage
        .apply_structural_command(&StructuralCommand::SetProperty {
            prim: body_prim,
            property: "primvars:displayColor".to_owned(),
            value: PropertyValue::Color3f([1.0, 0.0, 0.0]),
        })
        .expect("author displayColor onto the over");
    stage.drain_changes().expect("drain set-property");

    // remove-over must refuse — the opinion now carries real data.
    stage
        .apply_structural_command(&StructuralCommand::RemoveOverride {
            path: body.to_owned(),
        })
        .expect_err("remove-over must refuse an override carrying authored data");

    drop(stage);
    std::fs::remove_dir_all(dir).expect("cleanup isolated fixture");
}

/// Creating an opinion on a prim nested two composed levels below the
/// nearest root-authored ancestor authors a *fresh* `over` on the
/// intermediate ancestor too (`SdfCreatePrimInLayer` fills the whole chain).
///
/// Reprojecting only the leaf leaves the intermediate parent's cached
/// provenance stale — so the scene tree would keep painting it display-only.
/// Reprojecting the ancestor chain (what `StudioSession::create_opinion`
/// now does) refreshes every parent's `local_specifier` in lockstep.

#[test]
fn create_opinion_reprojects_the_whole_ancestor_chain() {
    let root = layerstack_fixture("opinion_chain_root.usda");
    layerstack_fixture("opinion_chain_asset.usda");
    let mut stage = StageService::open(&root).expect("open opinion-chain fixture");
    stage.project_topology().expect("topology");

    let specifier = |stage: &StageService, path: &str| {
        stage
            .topology()
            .iter()
            .find(|row| stage.path(row.path) == Some(path))
            .unwrap_or_else(|| panic!("topology row for {path}"))
            .local_specifier
    };

    let group = "/World/Widget/Group";
    let block = "/World/Widget/Group/Block";

    // Before: both interior prims are composed purely from the reference —
    // neither has a root-layer spec.
    assert_eq!(specifier(&stage, group), None, "group starts display-only");
    assert_eq!(specifier(&stage, block), None, "block starts display-only");

    // Author the opinion on the deep leaf; USD fills in the ancestor overs.
    stage
        .apply_structural_command(&StructuralCommand::CreateOverride {
            path: block.to_owned(),
        })
        .expect("create over on nested block");
    stage.drain_changes().expect("drain create-over");

    // A leaf-only reprojection (the old create_opinion behaviour) refreshes
    // the block but leaves the freshly-authored parent over invisible.
    stage
        .project_topology_paths(&[block.to_owned()])
        .expect("reproject leaf only");
    assert_eq!(
        specifier(&stage, block),
        Some(PrimSpecifier::Over),
        "leaf reprojection refreshes the block",
    );
    assert_eq!(
        specifier(&stage, group),
        None,
        "leaf-only reprojection must miss the fresh parent over (the bug)",
    );

    // Reprojecting the whole ancestor chain (the fix) flips the parent too.
    stage
        .project_topology_paths(&[
            block.to_owned(),
            group.to_owned(),
            "/World/Widget".to_owned(),
            "/World".to_owned(),
        ])
        .expect("reproject ancestor chain");
    assert_eq!(
        specifier(&stage, group),
        Some(PrimSpecifier::Over),
        "ancestor-chain reprojection flips the intermediate parent to over",
    );
    assert_eq!(
        specifier(&stage, block),
        Some(PrimSpecifier::Over),
        "block remains over after the chain reprojection",
    );
}

#[test]
fn layer_stack_surfaces_unloaded_payload_layers_deduped() {
    let f = fixture("hydration/payload_outer.usda");
    fixture("hydration/payload_inner.usda");
    // Open topology-only (LoadNone): the payload is *not* loaded, yet the
    // arc must still be discovered via the composition query's expanded
    // prim index.
    let mut stage = StageService::open_topology_only(&f).expect("open payload fixture (LoadNone)");
    stage.project_topology().expect("topology");

    let layers = stage.layer_stack().expect("layer_stack");
    let payloads: Vec<&LayerRecord> = layers
        .iter()
        .filter(|l| l.arc == LayerArcKind::Payload)
        .collect();
    // payload_outer.usda references payload_inner.usda from two prims
    // (/Scene/Asset and /Scene/AssetB); dedup collapses them to one row.
    assert_eq!(
        payloads.len(),
        1,
        "expected one deduped payload layer, got {:?}",
        layers.iter().map(|l| &l.identifier).collect::<Vec<_>>()
    );
    let inner = payloads[0];
    assert!(
        inner.identifier.ends_with("payload_inner.usda"),
        "payload id: {}",
        inner.identifier
    );
    assert_eq!(inner.arc_target_path.as_deref(), Some("/Asset"));
    assert!(
        !inner.is_loaded,
        "payload must report unloaded under LoadNone"
    );
}

// ── Layer-stack hierarchy tests ────────────────────────────────────────────
//
// Uses layerstack_hier_root.usda and its companions (sub, ref, ref_sub,
// outer, inner).  These tests verify that `parent_identifier` is set to the
// layer that *authors* each arc — not to the root stage or to the layer
// itself — and that the bridge never emits a layer as a subLayer child of
// itself (the self-re-emission bug fixed in emit_external_layer).

#[test]
#[ignore]
fn layer_stack_hierarchy_parent_identifier_correct_for_reference_sublayer() {
    let f = layerstack_fixture("layerstack_hier_root.usda");
    let mut stage = StageService::open(&f).expect("open hier fixture");
    stage.project_topology().expect("topology");

    let layers = stage.layer_stack().expect("layer_stack");

    // Sublayer of root → parent must be root, not empty or itself.
    let sub = layers
        .iter()
        .find(|l| l.identifier.ends_with("layerstack_hier_sub.usda"))
        .expect("sub layer present");
    assert_eq!(sub.arc, LayerArcKind::SubLayer);
    assert!(
        sub.parent_identifier.ends_with("layerstack_hier_root.usda"),
        "sub parent should be root, got: {}",
        sub.parent_identifier
    );

    // Referenced asset → parent must be root (the layer that authors the arc).
    let refr = layers
        .iter()
        .find(|l| l.identifier.ends_with("layerstack_hier_ref.usda"))
        .expect("ref layer present");
    assert_eq!(refr.arc, LayerArcKind::Reference);
    assert!(
        refr.parent_identifier
            .ends_with("layerstack_hier_root.usda"),
        "ref parent should be root, got: {}",
        refr.parent_identifier
    );

    // Sublayer of the referenced asset → parent must be the ref asset,
    // not the root stage.  This verifies that sublayers of external arcs are
    // correctly attributed to the external layer, not the root.
    let ref_sub = layers
        .iter()
        .find(|l| l.identifier.ends_with("layerstack_hier_ref_sub.usda"))
        .expect("ref sublayer present");
    assert_eq!(ref_sub.arc, LayerArcKind::SubLayer);
    assert!(
        ref_sub
            .parent_identifier
            .ends_with("layerstack_hier_ref.usda"),
        "ref sublayer parent should be the ref asset, got: {}",
        ref_sub.parent_identifier
    );
}

#[test]
#[ignore]
fn layer_stack_hierarchy_parent_identifier_correct_for_nested_payload() {
    let f = layerstack_fixture("layerstack_hier_root.usda");
    let mut stage = StageService::open(&f).expect("open hier fixture");
    stage.project_topology().expect("topology");

    let layers = stage.layer_stack().expect("layer_stack");

    // First-level payload → parent must be root.
    let outer = layers
        .iter()
        .find(|l| l.identifier.ends_with("layerstack_hier_outer.usda"))
        .expect("outer payload present");
    assert_eq!(outer.arc, LayerArcKind::Payload);
    assert!(
        outer
            .parent_identifier
            .ends_with("layerstack_hier_root.usda"),
        "outer payload parent should be root, got: {}",
        outer.parent_identifier
    );

    // Second-level (nested) payload → parent must be the outer payload,
    // not the root stage.
    let inner = layers
        .iter()
        .find(|l| l.identifier.ends_with("layerstack_hier_inner.usda"))
        .expect("inner payload present");
    assert_eq!(inner.arc, LayerArcKind::Payload);
    assert!(
        inner
            .parent_identifier
            .ends_with("layerstack_hier_outer.usda"),
        "inner payload parent should be the outer payload, got: {}",
        inner.parent_identifier
    );
}

#[test]
#[ignore]
fn layer_stack_no_layer_is_its_own_parent() {
    // Regression test for the emit_external_layer self-re-emission bug:
    // after emitting a payload/reference record, collect_layer_records was
    // called on the same layer, causing it to appear as a subLayer child of
    // itself.
    let f = layerstack_fixture("layerstack_hier_root.usda");
    let mut stage = StageService::open(&f).expect("open hier fixture");
    stage.project_topology().expect("topology");

    let layers = stage.layer_stack().expect("layer_stack");
    for layer in &layers {
        assert_ne!(
            layer.parent_identifier, layer.identifier,
            "layer {} has itself as parent (self-re-emission bug)",
            layer.identifier
        );
    }
}

#[test]
#[ignore]
fn layer_stack_hierarchy_depth_order_correct() {
    // Sublayers / references / payloads must be listed after their parent
    // in the flat output (pre-order DFS invariant from the C++ bridge).
    let f = layerstack_fixture("layerstack_hier_root.usda");
    let mut stage = StageService::open(&f).expect("open hier fixture");
    stage.project_topology().expect("topology");

    let layers = stage.layer_stack().expect("layer_stack");

    // Helper: index of the first record ending with `suffix`.
    let pos = |suffix: &str| {
        layers
            .iter()
            .position(|l| l.identifier.ends_with(suffix))
            .unwrap_or(usize::MAX)
    };

    let root_pos = pos("layerstack_hier_root.usda");
    let sub_pos = pos("layerstack_hier_sub.usda");
    let ref_pos = pos("layerstack_hier_ref.usda");
    let ref_sub_pos = pos("layerstack_hier_ref_sub.usda");
    let outer_pos = pos("layerstack_hier_outer.usda");
    let inner_pos = pos("layerstack_hier_inner.usda");

    // All layers must be present.
    assert!(root_pos < usize::MAX, "root missing");
    assert!(sub_pos < usize::MAX, "sub missing");
    assert!(ref_pos < usize::MAX, "ref missing");
    assert!(ref_sub_pos < usize::MAX, "ref_sub missing");
    assert!(outer_pos < usize::MAX, "outer missing");
    assert!(inner_pos < usize::MAX, "inner missing");

    // Pre-order: parent before children.
    assert!(root_pos < sub_pos, "root must precede sub");
    assert!(root_pos < ref_pos, "root must precede ref");
    assert!(ref_pos < ref_sub_pos, "ref must precede ref_sub");
    assert!(root_pos < outer_pos, "root must precede outer");
    assert!(outer_pos < inner_pos, "outer must precede inner");
}

#[test]
fn eval_attribute_returns_typed_value_for_authored_float() {
    let f = uber_fixture("uber_bridge_test.usda");
    let mut stage = StageService::open(&f).expect("open uber fixture");
    stage.project_topology().expect("topology");
    let plane = stage.prim_at_path("/Root/Plane").expect("plane prim");
    let result = stage
        .eval_attribute(plane, "primvars:displayColor", 0.0, true)
        .expect("eval_attribute call");
    // Plane has no displayColor authored — expect None (not an error)
    // We verify the call succeeds without panicking; authored attrs follow below.
    let _ = result;

    // Verify we can evaluate a guaranteed-authored attribute
    let result = stage
        .eval_attribute(plane, "faceVertexCounts", 0.0, true)
        .expect("eval faceVertexCounts")
        .expect("faceVertexCounts should be authored");
    assert_eq!(result.type_name, "int[]");
    assert!(!result.value_repr.is_empty());
}

#[test]
fn prim_is_a_detects_mesh_and_dome_light() {
    let f = uber_fixture("uber_bridge_test.usda");
    let mut stage = StageService::open(&f).expect("open uber fixture");
    stage.project_topology().expect("topology");

    let plane = stage.prim_at_path("/Root/Plane").expect("plane");
    assert!(stage.prim_is_a(plane, "Mesh").expect("prim_is_a Mesh"));
    assert!(!stage.prim_is_a(plane, "Sphere").expect("prim_is_a Sphere"));

    let sky = stage.prim_at_path("/Root/Sky").expect("dome light");
    assert!(
        stage
            .prim_is_a(sky, "DomeLight")
            .expect("prim_is_a DomeLight")
    );
}

#[test]
fn prim_has_api_detects_material_binding() {
    let f = uber_fixture("uber_bridge_test.usda");
    let mut stage = StageService::open(&f).expect("open uber fixture");
    stage.project_topology().expect("topology");

    let plane = stage.prim_at_path("/Root/Plane").expect("plane");
    assert!(
        stage
            .prim_has_api(plane, "MaterialBindingAPI")
            .expect("prim_has_api")
    );
    // DomeLight does not have SkelBindingAPI
    let sky = stage.prim_at_path("/Root/Sky").expect("sky");
    assert!(
        !stage
            .prim_has_api(sky, "SkelBindingAPI")
            .expect("prim_has_api false")
    );
}

#[test]
fn skel_summaries_finds_skeleton_and_animation() {
    let f = uber_fixture("uber_bridge_test.usda");
    let stage = StageService::open(&f).expect("open uber fixture");
    let summaries = stage.skel_summaries().expect("skel_summaries");
    assert!(
        !summaries.skeletons.is_empty(),
        "expected at least one skeleton"
    );
    let skel = summaries
        .skeletons
        .iter()
        .find(|s| s.path.contains("Skel"))
        .expect("Skel prim");
    assert_eq!(skel.joint_count, 3);
    assert!(skel.has_anim_query);

    assert!(
        !summaries.animations.is_empty(),
        "expected at least one animation"
    );
    let anim = &summaries.animations[0];
    assert!(anim.has_translations);
    assert!(anim.has_rotations);
    assert!(anim.has_scales);
    assert!((anim.start_time_code - 0.0).abs() < f64::EPSILON);
    assert!((anim.end_time_code - 24.0).abs() < f64::EPSILON);
}

#[test]
fn observer_checkpoint_and_rollback_are_stable_on_unchanged_stage() {
    let f = uber_fixture("uber_bridge_test.usda");
    let mut stage = StageService::open(&f).expect("open uber fixture");
    let cp1 = stage.observer_checkpoint().expect("checkpoint");
    let cp2 = stage.observer_checkpoint().expect("checkpoint again");
    assert_eq!(
        cp1, cp2,
        "checkpoint must be stable without intervening changes"
    );
    // rollback to the same checkpoint is a no-op
    stage.observer_rollback_to(cp1).expect("rollback no-op");
    let cp3 = stage
        .observer_checkpoint()
        .expect("checkpoint after rollback");
    assert_eq!(cp1, cp3);
}

#[test]
fn material_surface_wgsl_returns_stub_or_source_for_grey_material() {
    let f = uber_fixture("uber_bridge_test.usda");
    let mut stage = StageService::open(&f).expect("open uber fixture");
    stage.project_topology().expect("topology");
    let mat = stage
        .prim_at_path("/Root/Looks/Grey")
        .expect("Grey material");
    let wgsl = stage
        .material_surface_wgsl(mat)
        .expect("material_surface_wgsl");
    assert_eq!(wgsl.material_path, "/Root/Looks/Grey");
    // No error diagnostics must be present (prim not found / not a Material).
    let has_error_diag = wgsl
        .diagnostics
        .iter()
        .any(|d| d.contains("prim not found") || d.contains("not a Material"));
    assert!(
        !has_error_diag,
        "unexpected error diagnostic from material_surface_wgsl: {:#?}",
        wgsl.diagnostics
    );
    // Without materialx-wgsl feature the C++ fallback always emits a GLSL stub
    // into wgsl_source. With it, flat UsdPreviewSurface materials (no .mtlx
    // layer) return empty wgsl_source by contract — the renderer uses the PBR
    // uber-shader path instead. Only assert non-empty in the stub-only build.
    #[cfg(not(feature = "materialx-wgsl"))]
    {
        assert!(
            !wgsl.wgsl_source.is_empty(),
            "wgsl_source must not be empty without materialx-wgsl feature"
        );
        assert!(
            !wgsl.fingerprint.is_empty(),
            "fingerprint must not be empty without materialx-wgsl feature"
        );
    }
    // With materialx-wgsl: flat PBR → empty source is expected; verify the
    // diagnostic explains the absence rather than silently hiding a bug.
    #[cfg(feature = "materialx-wgsl")]
    if wgsl.wgsl_source.is_empty() {
        assert!(
            wgsl.diagnostics
                .iter()
                .any(|d| d.contains("no .mtlx layer") || d.contains("not a Material")),
            "empty wgsl_source with no explanatory diagnostic: {:#?}",
            wgsl.diagnostics
        );
    }
}

#[cfg(feature = "materialx-naga")]
#[test]
fn transpile_glsl_stub_produces_valid_wgsl() {
    // Verify that the naga GLSL→WGSL path successfully handles the C++ placeholder stub.
    // The stub is a valid (but trivial) GLSL fragment shader that outputs grey.
    let glsl = concat!(
        "// IDTX_MATERIALX_GLSL_STUB\n",
        "// material=/Root/Looks/Grey\n",
        "#version 450\n",
        "layout(location=0) out vec4 frag_color;\n",
        "void main() { frag_color = vec4(0.5, 0.5, 0.5, 1.0); }\n",
    );
    let result = transpile_glsl_to_wgsl(glsl);
    assert!(
        result.is_ok(),
        "naga transpile of GLSL stub must succeed; error: {:?}",
        result
    );
    let wgsl = result.unwrap();
    assert!(!wgsl.is_empty(), "transpiled WGSL must not be empty");
    // naga emits at least one fn declaration for the fragment entry point
    assert!(
        wgsl.contains("fn "),
        "transpiled WGSL must contain a function declaration: {wgsl}"
    );
}

/// Copy the `root_opinion_over_sublayer.usda` root layer **and** its
/// sublayer into a unique temp dir (same basenames so the relative
/// `@./…@` sublayer path still resolves), returning the opened stage plus
/// the two temp paths for cleanup. Authoring mutates the cached root layer
/// process-globally, so an isolated copy keeps parallel tests clean.

fn open_root_opinion_copy() -> (StageService, PathBuf, PathBuf) {
    let root_src = layerstack_fixture("root_opinion_over_sublayer.usda");
    let sub_src = layerstack_fixture("root_opinion_sublayer.usda");
    let dir = std::env::temp_dir().join(format!(
        "idtx-root-opinion-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let root_dst = dir.join("root_opinion_over_sublayer.usda");
    let sub_dst = dir.join("root_opinion_sublayer.usda");
    std::fs::copy(&root_src, &root_dst).expect("copy root layer");
    std::fs::copy(&sub_src, &sub_dst).expect("copy sublayer");
    let stage = StageService::open(&root_dst).expect("open isolated root-opinion fixture");
    (stage, root_dst, sub_dst)
}

/// Parse the leading float from a scalar `value_summary` (e.g. `"5"`,
/// `"1"`, `"1.0"`), tolerant of surrounding whitespace.

fn summary_f64(summary: &str) -> f64 {
    summary
        .trim()
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .find(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("value_summary is not a scalar float: {summary:?}"))
}

/// End-to-end for the inspector's **Remove opinion** affordance: a root-layer
/// override (`radius = 5`) shadows a sublayer's `radius = 1`. `inspect_prim`
/// must flag the property `root_authored`; snapshotting yields a non-empty
/// spec; clearing the root opinion reverts the composed value to the
/// sublayer's `1` and drops `root_authored`; restoring the snapshot brings
/// the `5` back. Mirrors the shell's snapshot → clear → (undo) restore flow.

#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn remove_root_opinion_reverts_to_sublayer_and_restores() {
    let (mut stage, root_dst, sub_dst) = open_root_opinion_copy();
    stage.project_topology().expect("topology");
    let ball = stage.prim_at_path("/World/Ball").expect("ball prim");

    // The root-layer override wins and is flagged root_authored.
    let radius_of = |stage: &mut StageService| -> PropertyRecord {
        stage
            .inspect_prim(ball)
            .expect("inspect")
            .into_iter()
            .find(|p| p.name == "radius")
            .expect("radius property")
    };
    let before = radius_of(&mut stage);
    assert!(
        before.root_authored,
        "root override must be flagged root_authored"
    );
    assert!(
        (summary_f64(&before.value_summary) - 5.0).abs() < 1e-6,
        "composed radius must be the root override 5; got {}",
        before.value_summary,
    );

    // Snapshot the root-layer spec (must be lossless, non-empty).
    let snapshot = stage
        .root_property_text(ball, "radius")
        .expect("snapshot root radius");
    assert!(
        !snapshot.is_empty(),
        "root_property_text must serialize the authored spec"
    );

    // Clear the root opinion → composed value falls back to the sublayer's 1.
    stage
        .replace_root_property_text(ball, "radius", "")
        .expect("clear root radius");
    let _ = stage.drain_changes();
    let after_clear = radius_of(&mut stage);
    assert!(
        !after_clear.root_authored,
        "root opinion must be gone after clear"
    );
    assert!(
        (summary_f64(&after_clear.value_summary) - 1.0).abs() < 1e-6,
        "composed radius must revert to sublayer 1; got {}",
        after_clear.value_summary,
    );

    // Restore the snapshot → the root override 5 is back (undo direction).
    stage
        .replace_root_property_text(ball, "radius", &snapshot)
        .expect("restore root radius");
    let _ = stage.drain_changes();
    let after_restore = radius_of(&mut stage);
    assert!(
        after_restore.root_authored,
        "restoring the snapshot re-authors the root opinion"
    );
    assert!(
        (summary_f64(&after_restore.value_summary) - 5.0).abs() < 1e-6,
        "composed radius must be 5 again after restore; got {}",
        after_restore.value_summary,
    );

    drop(stage);
    let _ = std::fs::remove_file(&root_dst);
    let _ = std::fs::remove_file(&sub_dst);
}

/// End-to-end for the Transform panel's **Reset Transform**: the prim's
/// `xformOp:translate` + `xformOpOrder` are authored in the root layer over
/// the sublayer's identity. Both are flagged `root_authored`; clearing every
/// transform op removes the local translate (composed local transform
/// reverts toward identity); restoring the snapshots brings the translate
/// back. Mirrors the shell's per-op snapshot → clear → (undo) restore loop.

#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn reset_transform_clears_root_ops_and_restores() {
    let (mut stage, root_dst, sub_dst) = open_root_opinion_copy();
    stage.project_topology().expect("topology");
    let ball = stage.prim_at_path("/World/Ball").expect("ball prim");

    // The shared opinion fixture authors radius only. Establish the
    // transform this test resets instead of assuming it is in the fixture.
    stage
        .apply_transform_edit(
            ball,
            TransformValues {
                translation: [3.0, 4.0, 5.0],
                rotation_xyz_degrees: [0.0; 3],
                scale: [1.0; 3],
            },
        )
        .expect("author transform under test");
    let before_matrix = stage
        .get_local_transform_f64(ball)
        .expect("local transform");

    let xform_props = |stage: &mut StageService| -> Vec<PropertyRecord> {
        stage
            .inspect_prim(ball)
            .expect("inspect")
            .into_iter()
            .filter(|p| p.name == "xformOpOrder" || p.name.starts_with("xformOp:"))
            .collect()
    };

    // Both transform properties are authored in the root layer.
    let before = xform_props(&mut stage);
    assert!(
        before.iter().any(|p| p.name == "xformOp:translate") && before.len() >= 2,
        "fixture must author xformOp:translate + xformOpOrder in the root layer"
    );
    assert!(
        before.iter().all(|p| p.root_authored),
        "every transform op must be flagged root_authored; got {before:?}"
    );

    // Snapshot then clear every transform op (the shell's reset loop).
    let mut snapshots: Vec<(String, String)> = Vec::new();
    for p in &before {
        let snap = stage
            .root_property_text(ball, &p.name)
            .expect("snapshot xform op");
        assert!(!snap.is_empty(), "{} must serialize", p.name);
        snapshots.push((p.name.clone(), snap));
    }
    for (name, _) in &snapshots {
        stage
            .replace_root_property_text(ball, name, "")
            .unwrap_or_else(|e| panic!("clear {name}: {e:?}"));
    }
    let _ = stage.drain_changes();

    let after_clear = xform_props(&mut stage);
    assert!(
        after_clear.iter().all(|p| !p.root_authored),
        "no transform op may remain root_authored after reset; got {after_clear:?}"
    );
    let cleared = stage
        .get_local_transform_f64(ball)
        .expect("cleared transform");
    assert!(cleared[12..15].iter().all(|v| v.abs() < 1e-12));

    // Restore every snapshot (undo direction) → the ops are root-authored again.
    for (name, snap) in &snapshots {
        stage
            .replace_root_property_text(ball, name, snap)
            .unwrap_or_else(|e| panic!("restore {name}: {e:?}"));
    }
    let _ = stage.drain_changes();
    let after_restore = xform_props(&mut stage);
    assert!(
        after_restore
            .iter()
            .any(|p| p.name == "xformOp:translate" && p.root_authored),
        "restoring snapshots must re-author the root transform ops; got {after_restore:?}"
    );
    let restored = stage
        .get_local_transform_f64(ball)
        .expect("restored transform");
    assert!(
        restored
            .iter()
            .zip(before_matrix)
            .all(|(a, b)| (a - b).abs() < 1e-12)
    );

    drop(stage);
    let _ = std::fs::remove_file(&root_dst);
    let _ = std::fs::remove_file(&sub_dst);
}
