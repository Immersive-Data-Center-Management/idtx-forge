//! A slow real resolver request must not run inside stage-owner application.
#![cfg(all(feature = "authoring", feature = "remote"))]
use std::{net::TcpListener, sync::mpsc, thread, time::{Duration, Instant}};
use idtx_collab::{client::CollabClient, proto::{self, BaseMessage, base_message}};
use idtx_studio_session::{StudioSession, TextureDecodeConfig, RenderWorldUnitScale};
use prost::Message as _;

#[path = "../../../tests/support/mod.rs"]
mod support;

#[test]
#[ignore = "links OpenUSD + headless wgpu + HTTP resolver"]
fn slow_asset_preparation_preserves_responsive_ordered_application() {
    let stage = support::usd_fixture("collaboration/stage.usda");
    let cache = support::scratch_dir("collab-assets");
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/asset.usda", server.server_addr().to_ip().unwrap());
    let (started, downloading) = mpsc::channel();
    let http = thread::spawn(move || {
        let request = server.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        started.send(()).unwrap();
        thread::sleep(Duration::from_millis(700));
        request.respond(tiny_http::Response::from_string(include_str!("../../../tests/fixtures/usd/collaboration/asset-b.usda"))).unwrap();
    });
    idtx_usd_resolver_http::install(cache.path()).unwrap();
    idtx_usd_resolver_http::set_network_policy(idtx_usd_resolver_http::NetworkPolicy::new(true, vec!["127.0.0.1".into()])
        .with_private_hosts(vec!["127.0.0.1".into()]));
    let (device, queue) = pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()).await.unwrap()
            .request_device(&wgpu::DeviceDescriptor::default()).await.unwrap()
    });
    let mut session = StudioSession::open_with_gpu(stage,
        device, queue, [32, 32], TextureDecodeConfig::FULL_RESOLUTION, RenderWorldUnitScale::METRE).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, wait) = mpsc::channel();
    let peer = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        for payload in [base_message::Payload::CreatePrimBroadcast(proto::CreatePrimBroadcast {
            parent_path: "/World".into(), prim_name: "Asset".into(), r#type: proto::PrimType::Ref as i32,
            reference_path: Some(url), ..Default::default()
        }), base_message::Payload::SnapshotComplete(proto::SnapshotComplete {})] {
            socket.send(tungstenite::Message::Binary(BaseMessage { payload: Some(payload), server_seq: 7, ..Default::default() }.encode_to_vec().into())).unwrap();
        }
        let _ = wait.recv_timeout(Duration::from_secs(5));
    });
    session.attach_collab(CollabClient::join(&format!("ws://{address}"), "test", "").unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut ticks_during_download = 0;
    let mut downloading_since = None;
    let mut worst = Duration::ZERO;
    while !session.collab_is_synced() {
        if downloading.try_recv().is_ok() { downloading_since = Some(Instant::now()); }
        let start = Instant::now();
        session.tick().unwrap();
        worst = worst.max(start.elapsed());
        if downloading_since.is_some_and(|start| start.elapsed() < Duration::from_millis(500)) {
            ticks_during_download += 1;
            assert!(!session.collab_is_synced());
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let _ = stop.send(());
    http.join().unwrap();
    peer.join().unwrap();
    assert!(session.get_world_transform("/World/Asset/FromB").is_some());
    assert!(ticks_during_download >= 10, "no responsive ticks during the download");
    assert!(worst < Duration::from_millis(100), "frame blocked for {worst:?}");
}
