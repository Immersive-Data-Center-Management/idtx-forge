#![allow(clippy::float_cmp)] // The WASM fixture writes exact integer counters.
use idtx_session_orchestration::{BundleContext, SessionSet};
use idtx_studio_session::{StudioSession, TextureDecodeConfig, RenderWorldUnitScale};
#[path = "../../../tests/support/mod.rs"]
mod support;

#[test]
#[ignore = "requires OpenUSD + headless GPU"]
fn mask_reentry_preserves_each_sessions_guest_store() {
    idtx_usd_extension::install().unwrap();
    let scratch = support::scratch_dir("runtime-session");
    let directory = scratch.path().canonicalize().unwrap();
    std::fs::write(directory.join("counter.wasm"), wat::parse_str(r#"(module
        (import "idtx" "idtx_entity_self" (func $self (result i64)))
        (import "idtx" "idtx_session_set_xform" (func $set (param i64 i32) (result i32)))
        (memory (export "memory") 1)
        (global $count (mut i32) (i32.const 0))
        (func (export "_idtx_abi_version") (result i32) i32.const 3)
        (func (export "_idtx_on_update") (param f32)
            (global.set $count (i32.add (global.get $count) (i32.const 1)))
            (f32.store (i32.const 0) (f32.convert_i32_s (global.get $count)))
            (f32.store (i32.const 24) (f32.const 1))
            (f32.store (i32.const 28) (f32.const 1))
            (f32.store (i32.const 32) (f32.const 1))
            (f32.store (i32.const 36) (f32.const 1))
            (drop (call $set (call $self) (i32.const 0)))))"#).unwrap()).unwrap();
    let (device, queue) = pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()).await.unwrap()
            .request_device(&wgpu::DeviceDescriptor::default()).await.unwrap()
    });
    let open = |name: &str| {
        let path = directory.join(format!("{name}.usda"));
        std::fs::write(&path, "#usda 1.0\ndef Xform \"World\"\n{\n    def Xform \"Actor\" {}\n}\n").unwrap();
        let mut session = StudioSession::open_with_gpu(&path, device.clone(), queue.clone(), [8, 8],
            TextureDecodeConfig::default(), RenderWorldUnitScale::METRE).unwrap();
        session.attach_behavior_source("/World/Actor", "counter.rs", "counter", "runtime", "Counter").unwrap();
        session.author_attribute("/World/Actor", "idtx:behavior:module", idtx_usd::PropertyValue::Asset("./counter.wasm".into())).unwrap();
        session.author_attribute("/World/Actor", "idtx:behavior:capabilities", idtx_usd::PropertyValue::TokenArray(vec!["stage_edit".into()])).unwrap();
        session.tick().unwrap();
        assert_eq!(BundleContext::new(directory.clone()).configure_session(&mut session), 1);
        session
    };
    let mut sessions = SessionSet::new(open("level"), Some(open("mask")));
    let tick = |session: &mut StudioSession| {
        session.tick_external_behaviors(0.02, idtx_behavior::XrSnapshot::default(), [0.0; 3].into(), idtx_behavior::RawInput::default());
        session.get_local_transform_f64("/World/Actor").unwrap()[12]
    };
    assert_eq!(tick(sessions.active_session_mut()), 1.0);
    sessions.show_transfer();
    assert_eq!(tick(sessions.active_session_mut()), 1.0);
    sessions.show_current();
    assert_eq!(tick(sessions.active_session_mut()), 2.0, "retained level's VM was restarted");
    sessions.show_transfer();
    assert_eq!(tick(sessions.active_session_mut()), 2.0, "resident mask's VM was restarted");
}
