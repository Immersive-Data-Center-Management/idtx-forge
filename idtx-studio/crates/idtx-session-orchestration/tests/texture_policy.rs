use std::{path::PathBuf, time::{Duration, Instant}};
use idtx_session_orchestration::{LevelTable, SessionSet, TransitionDriver};
use idtx_studio_session::{StudioSession, TextureDecodeConfig, RenderWorldUnitScale};

#[test]
#[ignore = "requires OpenUSD and headless GPU"]
fn boot_transfer_and_revisited_levels_keep_the_effective_policy() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testfiles/usd")
        .join(format!("texture-policy-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("level")).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    let _cleanup = Cleanup(dir.clone());
    let path = dir.join("level/level.usda");
    std::fs::write(&path, "#usda 1.0\ndef Xform \"World\" {}\n").unwrap();
    let (device, queue) = pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()).await.unwrap()
            .request_device(&wgpu::DeviceDescriptor::default()).await.unwrap()
    });
    let config = TextureDecodeConfig { max_dimension: Some(32), max_resident_bytes: 4096, ..TextureDecodeConfig::default() };
    let open = || StudioSession::open_with_gpu(&path, device.clone(), queue.clone(), [8, 8], config, RenderWorldUnitScale::METRE).unwrap();
    let mut sessions = SessionSet::new(open(), Some(open()));
    let mut driver = TransitionDriver::new(LevelTable::new(vec![path]), device, queue, [8, 8], Box::new(|_| {}), String::new());
    for _ in 0..3 {
        sessions.current_mut().post_pending_level_transition(idtx_behavior::LevelTransitionRequest { target: "level".into(), spawn: None });
        driver.poll(&mut sessions);
        let deadline = Instant::now() + Duration::from_secs(30);
        while driver.is_transitioning() {
            drop(driver.poll(&mut sessions));
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(driver.status(), idtx_session_orchestration::TransitionStatus::Completed);
        assert_eq!(sessions.current().texture_config(), config);
        assert_eq!(sessions.transfer_mut().unwrap().texture_config(), config);
    }
}
