//! Headless round-trip test for the Conductor "Create Behavior" authoring path.
//!
//! Exercises [`idtx_studio_session::StudioSession::create_behavior_source`]
//! against a real (opened) stage: authoring the *source* half of a behavior on a
//! prim must apply the `IdtxBehaviorAPI` schema and write `idtx:behavior:source`
//! + `idtx:behavior:uuid` + `idtx:behavior:kind`, leaving the compiled-module
//! attribute `idtx:behavior:module` **absent** (the `.wasm` is produced only
//! before preview / at packaging, in an overlay). The read accessor
//! [`StudioSession::behavior_source_path`] must recover the authored `.rs` path,
//! and [`StudioSession::prim_has_behavior`] must report the attachment.
//!
//! `#[ignore]` because it links the `OpenUSD` monolith and spins up a headless wgpu
//! device. Run with the USD env configured:
//!
//! ```bash
//! IDTX_USD_DIR=… DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib \
//!   cargo test -p idtx-studio-session --features authoring -- --ignored behavior_source
//! ```

#![cfg(feature = "authoring")]

use std::path::PathBuf;

use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/usd")
        .join(name)
}

/// Surfaceless wgpu device/queue for headless rendering (mirrors
/// `idtx-runtime-core::headless_device`).
fn headless_device() -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .expect("a headless wgpu adapter");
        adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("idtx-behavior-source-test"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await
            .expect("a headless wgpu device")
    })
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn create_behavior_source_authors_source_triple_not_module() {
    // Register the IDTX schema plugin so `ApplyApi("IdtxBehaviorAPI")` resolves
    // (the running Conductor does this at startup via the shell).
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let (device, queue) = headless_device();
    let mut session = StudioSession::open_with_gpu(
        fixture("hydration/basic-scene.usda"),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open basic-scene fixture");

    let prim = "/World/Cube";
    session
        .create_behavior_source(
            prim,
            "abc123def456",
            "runtime",
            "./behaviors/cube_behavior.rs",
        )
        .expect("author behavior source");

    // The source path round-trips through the read accessor.
    assert_eq!(
        session.behavior_source_path(prim).as_deref(),
        Some("./behaviors/cube_behavior.rs")
    );

    // The IdtxBehaviorAPI schema is applied — the canonical "has a behavior" test.
    assert!(
        session.prim_has_behavior(prim),
        "create must apply the IdtxBehaviorAPI schema (HasAPI)"
    );

    // inspect_prim shows the authored source triple.
    let props = session.inspect_prim(prim).expect("inspect");
    let authored: std::collections::HashSet<&str> = props
        .iter()
        .filter(|p| p.authored)
        .map(|p| p.name.as_str())
        .collect();
    assert!(
        authored.contains("idtx:behavior:source"),
        "source attribute must be authored"
    );
    assert!(
        authored.contains("idtx:behavior:uuid"),
        "uuid attribute must be authored"
    );
    assert!(
        authored.contains("idtx:behavior:kind"),
        "kind attribute must be authored"
    );

    // The compiled-module attachment must NOT be authored — no wasm at create.
    let module_authored = props
        .iter()
        .any(|p| p.name == "idtx:behavior:module" && p.authored);
    assert!(
        !module_authored,
        "create must not author the compiled-module attachment (idtx:behavior:module)"
    );
}

/// The Conductor "Attach Behavior…" session half: attaching an existing source
/// must apply `IdtxBehaviorAPI` and author the manifest scaffold
/// (source/uuid/kind + name + version) while leaving the compiled-module
/// attribute absent — the schema-attribute successor to the old `.wasm` attach
/// (there is no sidecar `idtx.toml`).
#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn attach_behavior_source_authors_scaffold_not_module() {
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let (device, queue) = headless_device();
    let mut session = StudioSession::open_with_gpu(
        fixture("hydration/basic-scene.usda"),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open basic-scene fixture");

    let prim = "/World/Cube";
    session
        .attach_behavior_source(
            prim,
            "./behaviors/spin.rs",
            "deadbeefcafef00d",
            "editor",
            "spin",
        )
        .expect("attach behavior source");

    // HasAPI reports the attachment, and the source path round-trips.
    assert!(
        session.prim_has_behavior(prim),
        "attach must apply the IdtxBehaviorAPI schema (HasAPI)"
    );
    assert_eq!(
        session.behavior_source_path(prim).as_deref(),
        Some("./behaviors/spin.rs")
    );

    // The scaffold attributes are authored; the compiled module is not.
    let props = session.inspect_prim(prim).expect("inspect");
    let authored: std::collections::HashSet<&str> = props
        .iter()
        .filter(|p| p.authored)
        .map(|p| p.name.as_str())
        .collect();
    for attr in [
        idtx_scene::behavior::attr::SOURCE,
        idtx_scene::behavior::attr::UUID,
        idtx_scene::behavior::attr::KIND,
        idtx_scene::behavior::attr::NAME,
        idtx_scene::behavior::attr::VERSION,
    ] {
        assert!(authored.contains(attr), "attach must author {attr}");
    }
    assert!(
        !authored.contains(idtx_scene::behavior::attr::MODULE),
        "attach must not author the compiled-module attachment (idtx:behavior:module)"
    );

    // The composed config reflects the authored kind/version.
    let cfg = session
        .read_composed_behavior_config(prim)
        .expect("read composed config");
    assert_eq!(cfg.kind.as_deref(), Some("editor"));
    assert_eq!(cfg.version.as_deref(), Some("0.1.0"));
}

/// Detaching a behavior must remove the `IdtxBehaviorAPI` schema **and** clear
/// every root-authored `idtx:behavior:*` opinion, so the prim is left pristine
/// (the counterpart of [`create_behavior_source_authors_source_triple_not_module`]).
/// A detach that removed only the schema would strand the source/uuid/kind/… and
/// the panel would keep showing the authored body (the bug this guards).
#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn detach_behavior_clears_schema_and_all_namespaced_attrs() {
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let (device, queue) = headless_device();
    let mut session = StudioSession::open_with_gpu(
        fixture("hydration/basic-scene.usda"),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open basic-scene fixture");

    let prim = "/World/Cube";
    session
        .create_behavior_source(prim, "abc123def456", "runtime", "./behaviors/cube_behavior.rs")
        .expect("author behavior source");

    // Author a few more namespaced config attrs so the test covers the whole
    // `idtx:behavior:*` namespace, not just the create-time source triple.
    session
        .set_behavior_scope(prim, idtx_scene::BehaviorScope::Stage)
        .expect("author scope");
    session
        .set_behavior_enabled(prim, false)
        .expect("author enabled");

    assert!(session.prim_has_behavior(prim), "precondition: behavior attached");

    // Detach.
    session.detach_behavior(prim).expect("detach behavior");

    // The schema is gone — the canonical "has a behavior" test flips to false.
    assert!(
        !session.prim_has_behavior(prim),
        "detach must remove the IdtxBehaviorAPI schema"
    );

    // No `idtx:behavior:*` opinion is left root-authored on the prim.
    let props = session.inspect_prim(prim).expect("inspect");
    let stranded: Vec<&str> = props
        .iter()
        .filter(|p| p.root_authored && p.name.starts_with("idtx:behavior:"))
        .map(|p| p.name.as_str())
        .collect();
    assert!(
        stranded.is_empty(),
        "detach must clear every idtx:behavior:* opinion; stranded: {stranded:?}"
    );
}

/// The session half of the Conductor "Overwrite Behavior" flow (the weaker-layer
/// / detached cases): [`StudioSession::read_composed_behavior_config`] must
/// capture the composed manifest config, and after a detach + a fresh
/// re-create, [`StudioSession::apply_behavior_config`] must re-author those
/// values as root opinions — with `IdtxBehaviorAPI` re-applied over the delete
/// the detach authored (the "ApplyApi over a delete" risk the plan flagged).
#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn overwrite_round_trip_captures_and_reauthors_config_over_detach() {
    use idtx_usd::PropertyValue;

    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let (device, queue) = headless_device();
    let mut session = StudioSession::open_with_gpu(
        fixture("hydration/basic-scene.usda"),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open basic-scene fixture");

    let prim = "/World/Cube";

    // Author a behavior with a non-default manifest config.
    session
        .create_behavior_source(prim, "abc123def456", "runtime", "./behaviors/cube_behavior.rs")
        .expect("author behavior source");
    session
        .set_behavior_scope(prim, idtx_scene::BehaviorScope::Stage)
        .expect("author scope");
    session
        .set_behavior_enabled(prim, false)
        .expect("author enabled");
    session
        .set_behavior_attribute(
            prim,
            idtx_scene::behavior::attr::CAPABILITIES,
            PropertyValue::TokenArray(vec!["input".to_owned(), "scene_query".to_owned()]),
        )
        .expect("author capabilities");
    session
        .set_behavior_attribute(
            prim,
            idtx_scene::behavior::attr::VERSION,
            PropertyValue::String("1.2.3".to_owned()),
        )
        .expect("author version");
    session
        .set_behavior_attribute(
            prim,
            idtx_scene::behavior::attr::MEMORY_MB,
            PropertyValue::Int(64),
        )
        .expect("author memory cap");

    // Capture the composed config (what Overwrite carries over).
    let snapshot = session
        .read_composed_behavior_config(prim)
        .expect("read composed behavior config");
    assert_eq!(snapshot.scope.as_deref(), Some("stage"));
    assert_eq!(snapshot.enabled, Some(false));
    assert!(snapshot.capabilities.iter().any(|c| c == "input"));
    assert!(snapshot.capabilities.iter().any(|c| c == "scene_query"));
    assert_eq!(snapshot.version.as_deref(), Some("1.2.3"));
    assert_eq!(snapshot.memory_mb, Some(64));

    // Detach — authors the API delete + clears the root opinions (the "detached"
    // precondition Overwrite recovers from).
    session.detach_behavior(prim).expect("detach behavior");
    assert!(
        !session.prim_has_behavior(prim),
        "precondition: behavior detached (API delete authored)"
    );

    // Overwrite: fresh schema re-apply + re-author the captured config. (The
    // shell also writes a new `.rs`; here we exercise the session halves.)
    session
        .create_behavior_source(prim, "newuuid789", "runtime", "./behaviors/cube_behavior_2.rs")
        .expect("re-create behavior source");
    session
        .apply_behavior_config(prim, &snapshot)
        .expect("apply carried-over config");

    // The API delete is overridden — HasAPI is true again.
    assert!(
        session.prim_has_behavior(prim),
        "re-applying IdtxBehaviorAPI must override the detach's delete"
    );

    // The carried-over config is now root-authored.
    let props = session.inspect_prim(prim).expect("inspect");
    let root_authored: std::collections::HashSet<&str> = props
        .iter()
        .filter(|p| p.root_authored)
        .map(|p| p.name.as_str())
        .collect();
    for attr in [
        idtx_scene::behavior::attr::SCOPE,
        idtx_scene::behavior::attr::ENABLED,
        idtx_scene::behavior::attr::CAPABILITIES,
        idtx_scene::behavior::attr::VERSION,
        idtx_scene::behavior::attr::MEMORY_MB,
    ] {
        assert!(
            root_authored.contains(attr),
            "overwrite must re-author {attr} as a root opinion; got {root_authored:?}"
        );
    }

    // The config values survived the round-trip.
    let after = session
        .read_composed_behavior_config(prim)
        .expect("read config after overwrite");
    assert_eq!(after.scope.as_deref(), Some("stage"));
    assert_eq!(after.enabled, Some(false));
    assert_eq!(after.version.as_deref(), Some("1.2.3"));
    assert_eq!(after.memory_mb, Some(64));
}

