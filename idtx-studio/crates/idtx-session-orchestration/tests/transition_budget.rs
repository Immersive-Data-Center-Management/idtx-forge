//! Slow real HTTP loads: responsive masks, bounded cancellation and readiness.
#![allow(clippy::float_cmp)] // The WASM fixture writes exact integer counters.
use std::{path::PathBuf, sync::{Arc, mpsc}, thread, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
use idtx_session_orchestration::{BundleContext, LevelTable, SessionSet, StageLoader, StageLoadError, TransitionDriver, TransitionStatus};
use idtx_studio_session::{StudioSession, TextureDecodeConfig, RenderWorldUnitScale};
struct Cleanup(PathBuf);
impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

#[test]
#[ignore = "requires OpenUSD + headless GPU + HTTP resolver"]
fn slow_loads_keep_masks_animated_and_cancel_or_timeout_without_cutover() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testfiles/usd")
        .join(format!("transition-budget-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&directory).unwrap();
    let _cleanup = Cleanup(directory.clone());
    idtx_usd_resolver_http::install(&directory.join("cache")).unwrap();
    idtx_usd_resolver_http::set_network_policy(idtx_usd_resolver_http::NetworkPolicy::new(true, vec!["127.0.0.1".into()])
        .with_private_hosts(vec!["127.0.0.1".into()]));
    idtx_usd_extension::install().unwrap();
    let counter = r#"(module
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
            (drop (call $set (call $self) (i32.const 0)))))"#;
    std::fs::write(directory.join("counter.wasm"), wat::parse_str(counter).unwrap()).unwrap();
    let cancelling = counter.replace("(memory", "(import \"idtx\" \"idtx_cancel_level_transition\" (func $cancel (result i32)))\n(memory")
        .replace("(global.set $count (i32.add (global.get $count) (i32.const 1)))", "(global.set $count (i32.add (global.get $count) (i32.const 1)))\n(if (i32.eq (global.get $count) (i32.const 4)) (then (drop (call $cancel))))");
    std::fs::write(directory.join("cancel.wasm"), wat::parse_str(cancelling).unwrap()).unwrap();
    let (device, queue) = pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()).await.unwrap().request_device(&wgpu::DeviceDescriptor::default()).await.unwrap()
    });
    for scenario in ["success", "cancel", "timeout"] {
        let context = BundleContext::new(directory.clone());
        context.state.set("score".into(), vec![7]).unwrap();
        let open = |name: &str| {
            let path = directory.join(format!("{scenario}-{name}.usda"));
            std::fs::write(&path, "#usda 1.0\ndef Xform \"World\"\n{\n    def Xform \"Actor\" {}\n}\n").unwrap();
            let mut session = StudioSession::open_with_gpu(&path, device.clone(), queue.clone(), [8, 8], TextureDecodeConfig::default(), RenderWorldUnitScale::METRE).unwrap();
            session.attach_behavior_source("/World/Actor", "counter.rs", "counter", "runtime", "Counter").unwrap();
            let module = if scenario == "cancel" && name == "mask" { "./cancel.wasm" } else { "./counter.wasm" };
            session.author_attribute("/World/Actor", "idtx:behavior:module", idtx_usd::PropertyValue::Asset(module.into())).unwrap();
            session.author_attribute("/World/Actor", "idtx:behavior:capabilities", idtx_usd::PropertyValue::TokenArray(vec!["stage_edit".into(), "level_control".into()])).unwrap();
            session.tick().unwrap();
            context.configure_session(&mut session);
            session.render_one_frame(false);
            session
        };
        let mut sessions = SessionSet::new(open("level"), Some(open("mask")));
        let original = sessions.current().id();
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let target = PathBuf::from(format!("http://{}/target/{scenario}.usda", server.server_addr().to_ip().unwrap()));
        let (sent, started) = mpsc::channel();
        let server = thread::spawn(move || {
            let request = server.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
            sent.send(()).unwrap();
            thread::sleep(Duration::from_millis(700));
            let _ = request.respond(tiny_http::Response::from_string("#usda 1.0\ndef Xform \"Incoming\" {}\n"));
        });
        let setup = context.clone();
        let mut driver = TransitionDriver::new(LevelTable::new(vec![target]), device.clone(), queue.clone(), [8, 8],
            Box::new(move |session| { setup.configure_session(session); }), String::new()).with_state(context.state.clone());
        if scenario == "timeout" { driver.set_timeout(Duration::from_millis(100)); }
        let prepared = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let preparation_flag = prepared.clone();
        driver.set_prepare(Arc::new(move |_| { preparation_flag.store(true, std::sync::atomic::Ordering::Release); }));
        sessions.current_mut().post_pending_level_transition(idtx_behavior::LevelTransitionRequest { target: "target".into(), spawn: None });
        driver.poll(&mut sessions);
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut frames = 0;
        let mut worst = Duration::ZERO;
        let mut worst_interval = Duration::ZERO;
        let mut previous_frame = Instant::now();
        let deadline = Instant::now() + Duration::from_secs(10);
        while driver.is_transitioning() {
            let start = Instant::now();
            worst_interval = worst_interval.max(start.duration_since(previous_frame));
            previous_frame = start;
            sessions.active_session_mut().tick_external_behaviors(0.01, idtx_behavior::XrSnapshot::default(), [0.0; 3].into(), idtx_behavior::RawInput::default());
            sessions.active_session_mut().render_one_frame(false);
            frames += 1;
            drop(driver.poll(&mut sessions));
            if scenario == "cancel" && frames == 4 {
                assert_eq!(driver.status(), TransitionStatus::Cancelled, "guest cancellation did not reach the driver");
                let busy = StageLoader::request(&directory.join("unused.usda"), device.clone(), queue.clone(), [8, 8], TextureDecodeConfig::default(), RenderWorldUnitScale::METRE, "");
                assert!(matches!(busy.try_recv(), std::task::Poll::Ready(Err(StageLoadError::Busy))));
            }
            worst = worst.max(start.elapsed());
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(worst < Duration::from_millis(100), "{scenario}: frame blocked for {worst:?}");
        assert!(worst_interval < Duration::from_millis(100), "{scenario}: frame gap {worst_interval:?}");
        if scenario == "success" {
            assert!(frames >= 10);
            assert_eq!(driver.status(), TransitionStatus::Completed);
            assert_ne!(sessions.current().id(), original);
            assert!(prepared.load(std::sync::atomic::Ordering::Acquire));
        } else {
            assert_eq!(sessions.current().id(), original);
            assert_eq!(driver.status(), if scenario == "cancel" { TransitionStatus::Cancelled } else { TransitionStatus::TimedOut });
            assert!(!prepared.load(std::sync::atomic::Ordering::Acquire));
        }
        assert_eq!(sessions.transfer_mut().unwrap().get_local_transform_f64("/World/Actor").unwrap()[12], f64::from(frames));
        assert_eq!(sessions.current().experience_state().get("score"), Some(vec![7]));
        #[cfg(feature = "metrics")]
        eprintln!("{scenario}: frames={frames}, worst={worst:?}, interval={worst_interval:?}, metrics={:?}", driver.metrics());
        server.join().unwrap();
        while StageLoader::active_loads() != 0 {
            assert!(Instant::now() < deadline, "cancelled worker did not release its slot");
            thread::sleep(Duration::from_millis(2));
        }
    }
}
