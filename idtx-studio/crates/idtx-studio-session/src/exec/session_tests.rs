//! Program refresh must not freeze live providers or recreate polling transport.
use super::*;
use crate::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

#[test]
#[ignore = "requires OpenUSD link and headless GPU"]
fn retained_pool_refreshes_live_provider_metadata_and_resumes_idle_gating() {
    idtx_usd_extension::install().unwrap();
    idtx_usd_exec::install().unwrap();
    let value = Arc::new(std::sync::atomic::AtomicU64::new(1));
    let calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let provider_value = Arc::clone(&value);
    let provider_calls = Arc::clone(&calls);
    assert!(idtx_usd_exec::env::register_provider_at(
        &idtx_usd_exec::plugin_dir(),
        "review_r11",
        move |_: &str| {
            provider_calls.fetch_add(1, Ordering::Relaxed);
            Some(provider_value.load(Ordering::Relaxed).to_string())
        }
    ));
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("stage.usda");
    let original = include_str!("../../../../tests/fixtures/usd/compute/exec_chain_scene.usda")
        .replace("float interval = 1", "float interval = 3600");
    std::fs::write(&path, &original).unwrap();
    let (device, queue) = pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .unwrap();
        adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .unwrap()
    });
    let mut session = StudioSession::open_with_gpu(
        &path,
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::default(),
        RenderWorldUnitScale::METRE,
    )
    .unwrap();
    session.tick_exec(false).unwrap();
    let pool = session.exec.as_ref().unwrap().pool.as_ref().unwrap();
    let identity = Arc::clone(&pool.shared);
    let workers = pool.worker_count();
    assert!(!session.exec.as_ref().unwrap().has_live_providers());
    // Drain any immediate first mock response. Subsequent polls are an hour
    // apart; no HTTP completion is involved in the live-provider proof.
    session.exec.as_ref().unwrap().drain_into(&session.service);

    let with_live = format!(
        "{}\n{}",
        original.trim_end().strip_suffix('}').unwrap(),
        r#"
    def Compute_Environment "Live" {
        string inputs:key = "review_r11:VALUE"
        string outputs:value = ""
    }
    def ComputeValueSink "Sink" {
        string string_value = ""
        string string_value.connect = </World/Live.outputs:value>
    }
}
"#
    );
    session.service.replace_root_layer_text(&with_live).unwrap();
    session.tick().unwrap();
    session.tick_exec(false).unwrap();
    assert!(session.exec.as_ref().unwrap().has_live_providers());
    for expected in [2, 3] {
        value.store(expected, Ordering::Relaxed);
        assert!(
            session.tick_exec(false).unwrap() > 0,
            "live input changes without a datasource response"
        );
        let sink = session.service.prim_at_path("/World/Sink").unwrap();
        let text = session
            .service
            .eval_attribute(sink, "string_value", 0.0, true)
            .unwrap()
            .unwrap()
            .value_repr;
        assert_eq!(text.trim_matches('"'), expected.to_string());
    }
    let pool = session.exec.as_ref().unwrap().pool.as_ref().unwrap();
    assert!(Arc::ptr_eq(&identity, &pool.shared));
    assert_eq!(pool.worker_count(), workers);
    assert!(!identity.cancel.load(Ordering::Relaxed));

    session.service.replace_root_layer_text(&original).unwrap();
    session.tick().unwrap();
    session.tick_exec(false).unwrap();
    assert!(!session.exec.as_ref().unwrap().has_live_providers());
    let before = calls.load(Ordering::Relaxed);
    assert_eq!(session.tick_exec(false).unwrap(), 0);
    assert_eq!(calls.load(Ordering::Relaxed), before);
    assert!(Arc::ptr_eq(
        &identity,
        &session.exec.as_ref().unwrap().pool.as_ref().unwrap().shared
    ));
}
