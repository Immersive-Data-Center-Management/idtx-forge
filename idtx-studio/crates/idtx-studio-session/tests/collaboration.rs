//! Real WebSocket → session → USD application contracts.
#![cfg(all(feature = "authoring", feature = "remote"))]
use std::{net::TcpListener, path::PathBuf, sync::mpsc, thread, time::{Duration, Instant}};
use idtx_collab::{client::CollabClient, proto::{self, BaseMessage, base_message, transform_update}};
use idtx_studio_session::{StudioSession, TextureDecodeConfig, RenderWorldUnitScale};
use prost::Message as _;

#[path = "../../../tests/support/mod.rs"]
mod support;

struct Peer {
    send: mpsc::Sender<Vec<BaseMessage>>,
    received: mpsc::Receiver<BaseMessage>,
    worker: Option<thread::JoinHandle<()>>,
    commit_started: mpsc::Receiver<()>,
    commit_finish: mpsc::Sender<bool>,
}

impl Peer {
    fn connect() -> (Self, CollabClient) {
        Self::connect_with_commit(false)
    }

    fn connect_with_commit(serve_commit: bool) -> (Self, CollabClient) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (send, receive) = mpsc::channel::<Vec<BaseMessage>>();
        let (received_send, received) = mpsc::channel();
        let (commit_tx, commit_started) = mpsc::channel();
        let (commit_finish, committed) = mpsc::channel();
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let http_stop = stop.clone();
            let http = serve_commit.then(|| thread::spawn(move || {
                use std::io::{Read, Write};
                listener.set_nonblocking(true).unwrap();
                while !http_stop.load(std::sync::atomic::Ordering::Acquire) {
                    if let Ok((mut stream, _)) = listener.accept() {
                        stream.set_nonblocking(false).unwrap();
                        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                        let mut headers = Vec::new();
                        while !headers.ends_with(b"\r\n\r\n") {
                            let mut byte = [0];
                            stream.read_exact(&mut byte).unwrap();
                            headers.push(byte[0]);
                            assert!(headers.len() < 16384);
                        }
                        let _ = commit_tx.send(());
                        let success = committed.recv_timeout(Duration::from_secs(3)).unwrap_or(false);
                        let response = if success { "HTTP/1.1 200 OK" } else { "HTTP/1.1 500 Failed" };
                        let _ = write!(stream, "{response}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                    thread::sleep(Duration::from_millis(2));
                }
            }));
            stream.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
            stream.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            loop {
                match receive.recv_timeout(Duration::from_millis(2)) {
                    Ok(frames) => {
                        if frames.is_empty() { break; }
                        for frame in frames {
                            socket.send(tungstenite::Message::Binary(frame.encode_to_vec().into())).unwrap();
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                match socket.read() {
                    Ok(tungstenite::Message::Binary(bytes)) => {
                        let _ = received_send.send(BaseMessage::decode(bytes).unwrap());
                    }
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(error)) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                    Err(_) => break,
                }
            }
            stop.store(true, std::sync::atomic::Ordering::Release);
            if let Some(http) = http { let _ = http.join(); }
        });
        let client = CollabClient::join(&format!("ws://{address}"), "test", "").unwrap();
        (Self { send, received, worker: Some(worker), commit_started, commit_finish }, client)
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.commit_finish.send(false);
        let _ = self.send.send(Vec::new());
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn immediate_commit_preserves_local_only_newer_edits_and_failure() {
    for (local_width, newer_transform, http_success) in [(false, false, true), (true, false, true), (false, true, true), (false, false, false)] {
        let mut session = open();
        if local_width {
            session.author_attribute("/World/Local", "localOnly", idtx_usd::PropertyValue::F64(99.0)).unwrap();
        }
        session.mark_stage_saved();
        let (peer, client) = Peer::connect_with_commit(true);
        peer.send.send(vec![snapshot(7)]).unwrap();
        session.attach_collab(client);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !session.collab_is_synced() {
            session.tick().unwrap();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        if local_width { session.clear_property_opinion("/World/Local", "localOnly").unwrap(); }
        session.author_transform("/World/Local", [8.0; 3], [0.0; 3], [1.0; 3]).unwrap();
        let started = Instant::now();
        session.commit_collab().unwrap(); // no tick between editing and commit
        assert!(started.elapsed() < Duration::from_millis(100));
        let request = peer.received.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(peer.commit_started.try_recv().is_err());
        let mut ack = frame(8, base_message::Payload::Ack(proto::Ack { ok: true, ..Default::default() }));
        ack.request_id = request.request_id;
        peer.send.send(vec![ack]).unwrap();
        loop {
            assert!(session.poll_collab_commit().is_none());
            if peer.commit_started.try_recv().is_ok() { break; }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        if newer_transform {
            session.author_transform("/World/Local", [9.0; 3], [0.0; 3], [1.0; 3]).unwrap();
        }
        peer.commit_finish.send(http_success).unwrap();
        let result = loop {
            if let Some(result) = session.poll_collab_commit() { break result; }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        };
        let clean = !local_width && !newer_transform && http_success;
        assert_eq!(result.is_ok(), http_success, "width={local_width} newer={newer_transform}: {result:?}");
        if let Ok(saved) = result { assert_eq!(saved, clean); }
        assert_eq!(session.is_stage_dirty(), !clean);
    }
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn immediate_create_is_dirty_and_commits_only_after_authoritative_application() {
    let mut session = open();
    session.mark_stage_saved();
    let (peer, client) = Peer::connect_with_commit(true);
    peer.send.send(vec![snapshot(7)]).unwrap();
    session.attach_collab(client);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !session.collab_is_synced() {
        session.tick().unwrap();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(session.request_remote_prim("/World", "Plane"));
    assert!(session.is_stage_dirty(), "close must notice a create that has not broadcast yet");
    session.commit_collab().unwrap();
    let request = peer.received.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(peer.commit_started.try_recv().is_err());
    let mut create = plane(4.0);
    create.server_seq = 8;
    let mut ack = frame(8, base_message::Payload::Ack(proto::Ack { ok: true, ..Default::default() }));
    ack.request_id = request.request_id;
    peer.send.send(vec![create, ack]).unwrap();
    loop {
        assert!(session.poll_collab_commit().is_none());
        if peer.commit_started.try_recv().is_ok() { break; }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(session.root_layer_snapshot().unwrap().contains("double width = 4"));
    peer.commit_finish.send(true).unwrap();
    loop {
        if let Some(result) = session.poll_collab_commit() { assert_eq!(result, Ok(true)); break; }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(!session.is_stage_dirty());
}

fn frame(seq: u64, payload: base_message::Payload) -> BaseMessage {
    BaseMessage { session_id: Some("test".into()), server_seq: seq, payload: Some(payload), request_id: 0 }
}

fn separate(path: &str, seq: u64) -> BaseMessage {
    frame(seq, base_message::Payload::XformBroadcast(proto::TransformBroadcast {
        client_id: "peer".into(), update: Some(proto::TransformUpdate {
            prim_path: path.into(), transform: Some(transform_update::Transform::Seperate(proto::SeparateTransform {
                translation: Some(proto::Vector3 { x: 4.0, y: 5.0, z: 6.0 }), ..Default::default()
            })), ..Default::default()
        }),
    }))
}

fn snapshot(seq: u64) -> BaseMessage {
    frame(seq, base_message::Payload::SnapshotComplete(proto::SnapshotComplete {}))
}

fn plane(width: f64) -> BaseMessage {
    frame(7, base_message::Payload::CreatePrimBroadcast(proto::CreatePrimBroadcast {
        parent_path: "/World".into(), prim_name: "Plane".into(), r#type: proto::PrimType::Plane as i32,
        width: Some(width), length: Some(2.0), ..Default::default()
    }))
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn replay_repairs_authoritative_attributes_and_placement() {
    let mut session = open();
    session.define_prim("/World/Plane", "Plane").unwrap();
    session.author_attribute("/World/Plane", "width", idtx_usd::PropertyValue::F64(99.0)).unwrap();
    session.author_transform("/World/Plane", [8.0; 3], [0.0; 3], [1.0; 3]).unwrap();
    let _peer = apply_frames(&mut session, vec![plane(4.0), snapshot(7)]);
    let text = session.root_layer_snapshot().unwrap();
    assert!(text.contains("double width = 4"), "authoritative replay did not repair width: {text}");
    assert_eq!(session.get_local_transform("/World/Plane").unwrap()[3], [0.0, 0.0, 0.0, 1.0]);
    assert!(session.collab_recovery_opinions().iter().any(|(path, text)|
        path == "/World/Plane" && text.contains("double width = 99")), "local intent must remain recoverable");
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn partial_replay_rolls_back_existing_opinions_and_children() {
    let mut session = open();
    session.define_prim("/World/Plane", "Plane").unwrap();
    session.define_prim("/World/Plane/LocalChild", "Xform").unwrap();
    session.author_attribute("/World/Plane", "width", idtx_usd::PropertyValue::F64(99.0)).unwrap();
    let before = session.root_layer_snapshot().unwrap();
    let (peer, client) = Peer::connect();
    peer.send.send(vec![plane(f64::NAN), snapshot(7)]).unwrap();
    session.attach_collab(client);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if session.tick().is_err() { break; }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(session.root_layer_snapshot().unwrap(), before);
    assert!(!session.collab_is_synced());
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn repaired_prerequisite_retries_failed_transform_and_following_create() {
    let mut session = open();
    let (peer, client) = Peer::connect();
    peer.send.send(vec![separate("/World/Missing", 7), plane(4.0), snapshot(7)]).unwrap();
    session.attach_collab(client);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if session.tick().is_err() { break; }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(!session.root_layer_snapshot().unwrap().contains("double width = 4"));
    session.define_prim("/World/Missing", "Xform").unwrap();
    session.retry_collab_application();
    assert!(!session.collab_is_synced());
    while !session.collab_is_synced() {
        session.tick().unwrap();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(session.get_local_transform("/World/Missing").unwrap()[3], [4.0, 5.0, 6.0, 1.0]);
    assert!(session.root_layer_snapshot().unwrap().contains("double width = 4"));
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn reference_replay_replaces_arc_and_preserves_unrelated_children() {
    for payload in [false, true] {
        let mut session = open();
        session.define_prim("/World/Asset", "Xform").unwrap();
        session.define_prim("/World/Asset/LocalChild", "Xform").unwrap();
        session.add_reference("/World/Asset", "./asset-a.usda", None).unwrap();
        session.tick().unwrap();
        let create = proto::CreatePrimBroadcast {
            parent_path: "/World".into(), prim_name: "Asset".into(), r#type: proto::PrimType::Ref as i32,
            reference_path: Some("./asset-b.usda".into()),
            ref_type: Some(if payload { proto::PrimRefType::Payload } else { proto::PrimRefType::Reference } as i32),
            ..Default::default()
        };
        let _peer = apply_frames(&mut session, vec![frame(7, base_message::Payload::CreatePrimBroadcast(create.clone())),
            frame(7, base_message::Payload::CreatePrimBroadcast(create)), snapshot(7)]);
        let text = session.root_layer_snapshot().unwrap();
        assert!(!text.contains("asset-a.usda"));
        assert_eq!(text.matches("asset-b.usda").count(), 1);
        assert!(text.contains("def \"Asset\""), "reference replay must clear the prior authored type opinion: {text}");
        assert!(text.contains("LocalChild"));
        assert!(session.get_world_transform("/World/Asset/FromB").is_some());
        session.tick().unwrap();
        let paths: Vec<_> = session.world().iter_entities().filter_map(|entity|
            entity.get::<idtx_scene::PrimNode>().map(|node| node.path.clone())).collect();
        assert!(!paths.iter().any(|path| path == "/World/Asset/FromA"), "replaced arc must remove stale ECS entities");
        assert!(paths.iter().any(|path| path == "/World/Asset/FromB"));
    }
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn unavailable_reference_can_be_repaired_and_retried() {
    let scratch = support::scratch_dir("collab-repair");
    let mut session = open();
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/usd/collaboration")
        .join(format!("repair-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    let _cleanup = Cleanup(directory.clone());
    let asset = directory.join("repaired.usda");
    let before = session.root_layer_snapshot().unwrap();
    let (peer, client) = Peer::connect();
    peer.send.send(vec![frame(7, base_message::Payload::CreatePrimBroadcast(proto::CreatePrimBroadcast {
        parent_path: "/World".into(), prim_name: "Repaired".into(), r#type: proto::PrimType::Ref as i32,
        reference_path: Some(asset.to_string_lossy().into_owned()), ..Default::default()
    })), plane(4.0), snapshot(7)]).unwrap();
    session.attach_collab(client);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if session.tick().is_err() { break; }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(session.root_layer_snapshot().unwrap(), before);
    assert!(!session.take_collab_notices().is_empty(), "preparation failure must be surfaced");
    std::fs::write(&asset, include_str!("../../../tests/fixtures/usd/collaboration/asset-b.usda")).unwrap();
    session.retry_collab_application();
    while !session.collab_is_synced() {
        session.tick().unwrap();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(session.get_world_transform("/World/Repaired/FromB").is_some());
    assert!(session.root_layer_snapshot().unwrap().contains("double width = 4"));
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn failed_reference_create_leaves_no_placeholder() {
    let mut session = open();
    let before = session.root_layer_snapshot().unwrap();
    let (peer, client) = Peer::connect();
    peer.send.send(vec![frame(7, base_message::Payload::CreatePrimBroadcast(proto::CreatePrimBroadcast {
        parent_path: "/World".into(), prim_name: "Broken".into(), r#type: proto::PrimType::Ref as i32,
        reference_path: Some(String::new()), ..Default::default()
    })), plane(4.0), snapshot(7)]).unwrap();
    session.attach_collab(client);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if session.tick().is_err() { break; }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(session.root_layer_snapshot().unwrap(), before, "partial create escaped rollback");
    assert!(!session.collab_is_synced());
}

fn apply_frames(session: &mut StudioSession, frames: Vec<BaseMessage>) -> Peer {
    let (peer, client) = Peer::connect();
    peer.send.send(frames).unwrap();
    session.attach_collab(client);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !session.collab_is_synced() {
        assert!(Instant::now() < deadline);
        session.tick().unwrap();
        thread::sleep(Duration::from_millis(2));
    }
    peer
}

fn assert_matrix(actual: [[f32; 4]; 4], expected: [[f32; 4]; 4]) {
    for (a, b) in actual.into_iter().flatten().zip(expected.into_iter().flatten()) {
        assert!((a - b).abs() < 1e-5, "matrix component {a} != {b}");
    }
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn mixed_axis_local_placement_matches_direct_usd_with_nonuniform_parents() {
    for parent in ["/World", "/World/Parent", "/World/Singular"] {
        let mut session = open();
        let expected_path = format!("{parent}/Expected");
        session.define_prim(&expected_path, "Xform").unwrap();
        session.author_transform(&expected_path, [1.0, 2.0, 3.0], [20.0, 30.0, 40.0], [2.0, 3.0, 4.0]).unwrap();
        let expected = session.get_world_transform(&expected_path).unwrap();
        session.tick().unwrap();
        let create = proto::CreatePrimBroadcast {
            parent_path: parent.into(), prim_name: "Placed".into(), r#type: proto::PrimType::Xform as i32,
            world_transform: Some(proto::create_prim_broadcast::WorldTransform::Seperate(proto::SeparateTransform {
                translation: Some(proto::Vector3 { x: 1.0, y: 2.0, z: 3.0 }),
                rotation: Some(proto::Vector3 { x: 20.0, y: 30.0, z: 40.0 }),
                scale: Some(proto::Vector3 { x: 2.0, y: 3.0, z: 4.0 }),
            })), ..Default::default()
        };
        let _peer = apply_frames(&mut session, vec![frame(7, base_message::Payload::CreatePrimBroadcast(create)), snapshot(7)]);
        assert_matrix(session.get_world_transform(&format!("{parent}/Placed")).unwrap(), expected);
    }
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn inbound_matrix_preserves_reflection_and_shear() {
    let matrix = [-1.0, 0.0, 0.0, 0.0, 0.25, 2.0, 0.0, 0.0, 0.125, -0.5, 3.0, 0.0, 4.0, 5.0, 6.0, 1.0];
    let mut session = open();
    let update = proto::TransformBroadcast {
        client_id: "peer".into(), update: Some(proto::TransformUpdate {
            prim_path: "/World/Local".into(),
            transform: Some(transform_update::Transform::Matrix(idtx_collab::conv::wire_from_usd(matrix))),
            ..Default::default()
        }),
    };
    let _peer = apply_frames(&mut session, vec![frame(7, base_message::Payload::XformBroadcast(update)), snapshot(7)]);
    let expected = std::array::from_fn(|row| std::array::from_fn(|col| matrix[row * 4 + col] as f32));
    assert_matrix(session.get_local_transform("/World/Local").unwrap(), expected);
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn invalid_local_placement_rejects_before_creating_any_spec() {
    for parent in ["/World/Singular", "/World"] {
        let mut session = open();
        let before = session.root_layer_snapshot().unwrap();
        let create = proto::CreatePrimBroadcast {
            parent_path: parent.into(), prim_name: "Invalid".into(), r#type: proto::PrimType::Xform as i32,
            world_transform: Some(proto::create_prim_broadcast::WorldTransform::Seperate(proto::SeparateTransform {
                translation: Some(proto::Vector3 { x: f64::NAN, y: 2.0, z: 3.0 }), ..Default::default()
            })), ..Default::default()
        };
        let (peer, client) = Peer::connect();
        peer.send.send(vec![frame(7, base_message::Payload::CreatePrimBroadcast(create)), separate("/World/Local", 8), snapshot(8)]).unwrap();
        session.attach_collab(client);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if session.tick().is_err() { break; }
            assert!(Instant::now() < deadline, "expected invalid placement rejection");
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(before, session.root_layer_snapshot().unwrap(), "invalid input must not leave a placeholder or alter later targets");
        assert!(!session.collab_is_synced());
        assert_eq!(session.detach_collab().unwrap().applied_seq(), 0);
    }
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn separate_transform_on_pivot_uses_equivalent_exact_matrix() {
    let mut session = open();
    let _peer = apply_frames(&mut session, vec![separate("/World/Pivot", 7), snapshot(7)]);
    let expected = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 4.0, 5.0, 6.0, 1.0];
    assert_eq!(session.get_local_transform_f64("/World/Pivot").unwrap(), expected);
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn outbound_matrix_never_narrows_to_float() {
    let mut session = open();
    let mut matrix = [1.0, 0.0, 0.0, 0.0, 0.25, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
    let update = proto::TransformBroadcast {
        client_id: "peer".into(), update: Some(proto::TransformUpdate {
            prim_path: "/World/Local".into(),
            transform: Some(transform_update::Transform::Matrix(idtx_collab::conv::wire_from_usd(matrix))),
            ..Default::default()
        }),
    };
    let peer = apply_frames(&mut session, vec![frame(7, base_message::Payload::XformBroadcast(update)), snapshot(7)]);
    session.tick().unwrap(); // drain the matrix-op resync and its echo guard
    matrix[12] = 1_000_000_000.123_456_8;
    matrix[13] = 0.000_000_000_012_345;
    session.author_matrix_transform("/World/Local", matrix).unwrap();
    session.tick().unwrap();
    let frame = peer.received.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(frame.server_seq, 7);
    let Some(base_message::Payload::XformUpdate(update)) = frame.payload else { panic!("expected transform update") };
    let Some(transform_update::Transform::Matrix(wire)) = update.transform else { panic!("expected exact matrix") };
    assert_eq!(idtx_collab::conv::usd_from_wire(&wire), matrix);
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn local_edit_after_remote_value_is_not_mistaken_for_an_echo() {
    let mut session = open();
    let peer = apply_frames(&mut session, vec![separate("/World/Local", 7), snapshot(7)]);
    session.author_transform("/World/Local", [7.0, 8.0, 9.0], [20.0, 30.0, 40.0], [2.0, 3.0, 4.0]).unwrap();
    session.tick().unwrap();
    let frame = peer.received.recv_timeout(Duration::from_secs(3)).unwrap();
    let Some(base_message::Payload::XformUpdate(update)) = frame.payload else { panic!("expected transform update") };
    let Some(transform_update::Transform::Seperate(trs)) = update.transform else { panic!("expected exact authored TRS") };
    assert_eq!(trs.translation, Some(proto::Vector3 { x: 7.0, y: 8.0, z: 9.0 }));
    assert_eq!(trs.rotation, Some(proto::Vector3 { x: 20.0, y: 30.0, z: 40.0 }));
    assert_eq!(trs.scale, Some(proto::Vector3 { x: 2.0, y: 3.0, z: 4.0 }));
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn transform_opinion_replay_publishes_against_the_applied_peer_prefix() {
    let mut session = open();
    let peer = apply_frames(&mut session, vec![snapshot(7)]);
    let before = session.root_transform_snapshot("/World/Local").unwrap();
    let expected_matrix = session.get_local_transform_f64("/World/Local").unwrap();
    session.author_transform("/World/Local", [7.0, 8.0, 9.0], [20.0, 30.0, 40.0], [2.0, 3.0, 4.0]).unwrap();
    let after = session.root_transform_snapshot("/World/Local").unwrap();
    let request = peer.received.recv_timeout(Duration::from_secs(3)).unwrap();
    let mut ack = frame(8, base_message::Payload::Ack(proto::Ack { ok: true, ..Default::default() }));
    ack.request_id = request.request_id;
    let mut fence = plane(12.0);
    fence.server_seq = 8;
    peer.send.send(vec![ack, fence]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !session.root_layer_snapshot().unwrap().contains("double width = 12") {
        session.tick().unwrap();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    session.replay_root_transform("/World/Local", &after, &before).unwrap();
    assert_eq!(session.root_transform_snapshot("/World/Local").unwrap(), before);
    assert_eq!(session.get_local_transform_f64("/World/Local").unwrap(), expected_matrix);
    assert!(session.root_layer_snapshot().unwrap().contains("double width = 12"));
    let undo = peer.received.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(undo.server_seq, 8);
    assert!(matches!(undo.payload, Some(base_message::Payload::XformUpdate(_))));
}

fn open() -> StudioSession {
    let stage = support::usd_fixture("collaboration/stage.usda");
    support::usd_fixture("collaboration/asset-a.usda");
    support::usd_fixture("collaboration/asset-b.usda");
    let (device, queue) = pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()).await.unwrap()
            .request_device(&wgpu::DeviceDescriptor::default()).await.unwrap()
    });
    StudioSession::open_with_gpu(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/usd/collaboration/stage.usda"),
        device, queue, [32, 32], TextureDecodeConfig::FULL_RESOLUTION, RenderWorldUnitScale::METRE).unwrap()
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn snapshot_is_synced_only_after_real_usd_application() {
    let mut session = open();
    let (peer, client) = Peer::connect();
    peer.send.send(vec![separate("/World/Local", 7), snapshot(7)]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while client.next_inbound().is_none() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(client.applied_seq(), 0);
    assert!(!client.is_synced());
    session.attach_collab(client);
    while !session.collab_is_synced() {
        assert!(Instant::now() < deadline);
        session.tick().unwrap();
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(session.get_local_transform("/World/Local").unwrap()[3], [4.0, 5.0, 6.0, 1.0]);
    let client = session.detach_collab().unwrap();
    assert_eq!(client.applied_seq(), 7);
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn failed_usd_edit_cannot_advance_to_the_following_snapshot() {
    for target in ["/World/Scope", "/World/Missing"] {
    let mut session = open();
    let (peer, client) = Peer::connect();
    peer.send.send(vec![separate(target, 7), separate("/World/Local", 8), snapshot(8)]).unwrap();
    session.attach_collab(client);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if session.tick().is_err() { break; }
        assert!(Instant::now() < deadline, "expected application failure for {target}");
        thread::sleep(Duration::from_millis(2));
    }
    assert!(!session.collab_is_synced());
    assert!(!session.take_collab_notices().is_empty());
    let client = session.detach_collab().unwrap();
    assert_eq!(client.applied_seq(), 0);
    assert!(client.next_inbound().is_none(), "failed prefix is stopped, not skipped");
    }
}

fn double_attribute(name: &str, value: f64) -> proto::PrimAttribute {
    proto::PrimAttribute { name: name.into(), type_name: "double".into(),
        value: Some(proto::AttributeValue { doubles: vec![value], ..Default::default() }) }
}

fn attributes(path: &str, seq: u64, values: Vec<proto::PrimAttribute>) -> BaseMessage {
    frame(seq, base_message::Payload::SetPrimAttributesBroadcast(proto::SetPrimAttributesBroadcast {
        prim_path: path.into(), attributes: values,
    }))
}

fn attribute_ack(request: &BaseMessage, seq: u64, failed: Vec<proto::AttributeError>) -> BaseMessage {
    let Some(base_message::Payload::SetPrimAttributes(update)) = &request.payload else { panic!("expected attribute request"); };
    let mut message = frame(seq, base_message::Payload::SetPrimAttributesAck(proto::SetPrimAttributesAck {
        prim_path: update.prim_path.clone(), failed,
    }));
    message.request_id = request.request_id;
    message
}

fn pump_until(session: &mut StudioSession, mut done: impl FnMut(&StudioSession) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(session) {
        session.tick().unwrap();
        assert!(Instant::now() < deadline, "collaboration condition timed out");
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn attribute_predictions_reconcile_equal_foreign_values_and_overlapping_failures() {
    for reverse in [false, true] {
        let mut session = open();
        session.author_attribute("/World/Local", "x", idtx_usd::PropertyValue::F64(0.0)).unwrap();
        let (peer, client) = Peer::connect();
        peer.send.send(vec![snapshot(7)]).unwrap();
        session.attach_collab(client);
        pump_until(&mut session, StudioSession::collab_is_synced);
        session.author_attribute("/World/Local", "x", idtx_usd::PropertyValue::F64(1.0)).unwrap();
        session.author_attribute("/World/Local", "x", idtx_usd::PropertyValue::F64(2.0)).unwrap();
        let first = peer.received.recv_timeout(Duration::from_secs(3)).unwrap();
        let second = peer.received.recv_timeout(Duration::from_secs(3)).unwrap();
        let failure = || vec![proto::AttributeError { name: "x".into(), reason: proto::AttributeFailureReason::NotAuthorable as i32, detail: String::new() }];
        let mut acks = vec![attribute_ack(&first, 8, failure()), attribute_ack(&second, 0, failure())];
        if reverse { acks.reverse(); }
        let mut frames = vec![attributes("/World/Local", 8, vec![double_attribute("x", 1.0)])];
        frames.extend(acks);
        peer.send.send(frames).unwrap();
        pump_until(&mut session, |session| session.stage().read_sync_attribute("/World/Local", "x").unwrap().unwrap().value.doubles == [1.0]);
        assert!(session.collab_is_synced());
        assert_eq!(session.take_collab_notices().iter().filter(|notice| notice.contains("Server rejected attribute")).count(), 2);
        for _ in 0..3 { session.tick().unwrap(); }
        assert!(peer.received.recv_timeout(Duration::from_millis(100)).is_err(), "authoritative reconciliation must not echo");
    }
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn zero_sequence_attribute_snapshot_preserves_role_arrays_before_deactivation() {
    let mut session = open();
    let (peer, client) = Peer::connect();
    let mut create = plane(4.0);
    create.server_seq = 0;
    let color = proto::PrimAttribute { name: "primvars:displayColor".into(), type_name: "color3f".into(),
        value: Some(proto::AttributeValue { doubles: vec![0.25, 0.5, 0.75], is_array: true, ..Default::default() }) };
    peer.send.send(vec![create, attributes("/World/Plane", 0, vec![double_attribute("width", 9.0), color]),
        frame(0, base_message::Payload::SetPrimActiveBroadcast(proto::SetPrimActiveBroadcast { prim_path: "/World/Plane".into(), active: false })), snapshot(0)]).unwrap();
    session.attach_collab(client);
    pump_until(&mut session, StudioSession::collab_is_synced);
    assert!(session.stage().prim_at_path("/World/Plane").is_none());
    let color = session.stage().read_sync_attribute("/World/Plane", "primvars:displayColor").unwrap().unwrap();
    assert_eq!(color.type_name, "color3f");
    assert!(color.value.is_array);
    assert_eq!(color.value.doubles, [0.25, 0.5, 0.75]);
    assert_eq!(session.stage().read_sync_attribute("/World/Plane", "width").unwrap().unwrap().value.doubles, [9.0]);
    for _ in 0..3 { session.tick().unwrap(); }
    assert!(peer.received.recv_timeout(Duration::from_millis(100)).is_err());
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn immediate_attribute_commit_waits_for_echo_and_ack_and_marks_saved() {
    let mut session = open();
    session.mark_stage_saved();
    let (peer, client) = Peer::connect_with_commit(true);
    peer.send.send(vec![snapshot(7)]).unwrap();
    session.attach_collab(client);
    pump_until(&mut session, StudioSession::collab_is_synced);
    session.author_attribute("/World/Local", "width", idtx_usd::PropertyValue::F64(9.0)).unwrap();
    session.commit_collab().unwrap();
    let request = peer.received.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(peer.commit_started.try_recv().is_err());
    peer.send.send(vec![attributes("/World/Local", 8, vec![double_attribute("width", 9.0)]), attribute_ack(&request, 8, Vec::new())]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(session.poll_collab_commit().is_none());
        if peer.commit_started.try_recv().is_ok() { break; }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    peer.commit_finish.send(true).unwrap();
    loop {
        if let Some(result) = session.poll_collab_commit() { assert_eq!(result, Ok(true)); break; }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(!session.is_stage_dirty());
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn rejected_attribute_restores_absence_metadata_block_and_time_samples_exactly() {
    for property in ["", "custom double x (documentation = \"keep\")", "custom double x = None",
        "custom double x.timeSamples = {1: 2, 2: 4}"] {
        let mut session = open();
        if !property.is_empty() {
            let text = format!("#usda 1.0\nover \"World\" {{\n    over \"Local\" {{\n        {property}\n    }}\n}}\n");
            session.restore_property_opinion("/World/Local", "x", &text).unwrap();
        }
        let before = session.snapshot_root_property("/World/Local", "x").unwrap();
        let (peer, client) = Peer::connect();
        peer.send.send(vec![snapshot(7)]).unwrap();
        session.attach_collab(client);
        pump_until(&mut session, StudioSession::collab_is_synced);
        session.author_attribute("/World/Local", "x", idtx_usd::PropertyValue::F64(9.0)).unwrap();
        let request = peer.received.recv_timeout(Duration::from_secs(3)).unwrap();
        peer.send.send(vec![attribute_ack(&request, 0, vec![proto::AttributeError { name: "x".into(),
            reason: proto::AttributeFailureReason::QueueFull as i32, detail: String::new() }])]).unwrap();
        pump_until(&mut session, |session| session.snapshot_root_property("/World/Local", "x").unwrap() == before);
        assert!(session.collab_is_synced());
        assert_eq!(session.snapshot_root_property("/World/Local", "x").unwrap(), before, "{property}");
    }
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn committing_an_attribute_does_not_mark_a_raw_transform_edit_saved() {
    let mut session = open();
    session.mark_stage_saved();
    let (peer, client) = Peer::connect_with_commit(true);
    peer.send.send(vec![snapshot(7)]).unwrap();
    session.attach_collab(client);
    pump_until(&mut session, StudioSession::collab_is_synced);
    let before = session.root_transform_snapshot("/World/Local").unwrap();
    let desired = before.replace("xformOp:translate = (0, 0, 0)", "xformOp:translate = (3, 4, 5)");
    assert_ne!(before, desired);
    session.apply_root_layer_delta(&before, &desired).unwrap();
    session.author_attribute("/World/Local", "width", idtx_usd::PropertyValue::F64(9.0)).unwrap();
    session.commit_collab().unwrap();
    let request = peer.received.recv_timeout(Duration::from_secs(3)).unwrap();
    peer.send.send(vec![attributes("/World/Local", 8, vec![double_attribute("width", 9.0)]), attribute_ack(&request, 8, Vec::new())]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(session.poll_collab_commit().is_none());
        // A later notice publication does not retroactively cover the raw edit
        // in the commit barrier that was captured before publication.
        while let Ok(transform) = peer.received.try_recv() {
            let mut ack = frame(9, base_message::Payload::Ack(proto::Ack { ok: true, ..Default::default() }));
            ack.request_id = transform.request_id;
            peer.send.send(vec![ack]).unwrap();
        }
        if peer.commit_started.try_recv().is_ok() { break; }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    peer.commit_finish.send(true).unwrap();
    loop {
        if let Some(result) = session.poll_collab_commit() { assert_eq!(result, Ok(false)); break; }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(session.is_stage_dirty());
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn remote_nan_default_does_not_echo_from_its_usd_notice() {
    let mut session = open();
    let (peer, client) = Peer::connect();
    peer.send.send(vec![attributes("/World/Local", 7, vec![double_attribute("customNan", f64::NAN)]), snapshot(7)]).unwrap();
    session.attach_collab(client);
    pump_until(&mut session, StudioSession::collab_is_synced);
    assert!(session.stage().read_sync_attribute("/World/Local", "customNan").unwrap().unwrap().value.doubles[0].is_nan());
    for _ in 0..4 { session.tick().unwrap(); }
    assert!(peer.received.recv_timeout(Duration::from_millis(100)).is_err(), "NaN authority must not become a local edit");
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn pending_attribute_completion_preserves_later_local_clear_and_metadata() {
    for clear in [false, true] {
        let mut session = open();
        session.author_attribute("/World/Local", "x", idtx_usd::PropertyValue::F64(1.0)).unwrap();
        let (peer, client) = Peer::connect();
        peer.send.send(vec![snapshot(7)]).unwrap();
        session.attach_collab(client);
        pump_until(&mut session, StudioSession::collab_is_synced);
        session.author_attribute("/World/Local", "x", idtx_usd::PropertyValue::F64(2.0)).unwrap();
        let request = peer.received.recv_timeout(Duration::from_secs(3)).unwrap();
        if clear { session.clear_property_opinion("/World/Local", "x").unwrap(); }
        else {
            session.restore_property_opinion("/World/Local", "x",
                "#usda 1.0\nover \"World\" {\n    over \"Local\" {\n        custom double x = 2 (documentation = \"keep later metadata\")\n    }\n}\n").unwrap();
        }
        peer.send.send(vec![attributes("/World/Local", 8, vec![double_attribute("x", 2.0)]), attribute_ack(&request, 8, Vec::new()),
            attributes("/World/Local", 9, vec![double_attribute("fence", 9.0)])]).unwrap();
        pump_until(&mut session, |session| session.stage().read_sync_attribute("/World/Local", "fence").unwrap().is_some());
        let text = session.snapshot_root_property("/World/Local", "x").unwrap();
        if clear { assert!(text.is_empty(), "later local clear was replaced: {text}"); }
        else { assert!(text.contains("keep later metadata"), "later metadata was replaced: {text}"); }
    }
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn pending_attribute_failure_preserves_later_value_blocks_and_samples() {
    for special in ["custom double x = None", "custom double x.timeSamples = {1: 5, 2: 6}"] {
        let mut session = open();
        session.author_attribute("/World/Local", "x", idtx_usd::PropertyValue::F64(1.0)).unwrap();
        let (peer, client) = Peer::connect();
        peer.send.send(vec![snapshot(7)]).unwrap();
        session.attach_collab(client);
        pump_until(&mut session, StudioSession::collab_is_synced);
        session.author_attribute("/World/Local", "x", idtx_usd::PropertyValue::F64(2.0)).unwrap();
        let request = peer.received.recv_timeout(Duration::from_secs(3)).unwrap();
        let source = format!("#usda 1.0\nover \"World\" {{\n    over \"Local\" {{\n        {special}\n    }}\n}}\n");
        session.restore_property_opinion("/World/Local", "x", &source).unwrap();
        let expected = session.snapshot_root_property("/World/Local", "x").unwrap();
        peer.send.send(vec![attribute_ack(&request, 0, vec![proto::AttributeError { name: "x".into(),
            reason: proto::AttributeFailureReason::QueueFull as i32, detail: String::new() }]),
            attributes("/World/Local", 9, vec![double_attribute("fence", 9.0)])]).unwrap();
        pump_until(&mut session, |session| session.stage().read_sync_attribute("/World/Local", "fence").unwrap().is_some());
        assert_eq!(session.snapshot_root_property("/World/Local", "x").unwrap(), expected, "{special}");
    }
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn rejected_combined_default_and_metadata_edit_keeps_metadata_without_prediction() {
    for initially_authored in [false, true] {
        let mut session = open();
        if initially_authored {
            session.author_attribute("/World/Local", "x", idtx_usd::PropertyValue::F64(1.0)).unwrap();
        }
        let (peer, client) = Peer::connect();
        peer.send.send(vec![snapshot(7)]).unwrap();
        session.attach_collab(client);
        pump_until(&mut session, StudioSession::collab_is_synced);
        session.restore_property_opinion("/World/Local", "x",
            "#usda 1.0\nover \"World\" {\n    over \"Local\" {\n        custom double x = 3 (documentation = \"local metadata\")\n    }\n}\n").unwrap();
        let request = peer.received.recv_timeout(Duration::from_secs(3)).unwrap();
        peer.send.send(vec![attribute_ack(&request, 0, vec![proto::AttributeError { name: "x".into(),
            reason: proto::AttributeFailureReason::QueueFull as i32, detail: String::new() }]),
            attributes("/World/Local", 8, vec![double_attribute("fence", 8.0)])]).unwrap();
        pump_until(&mut session, |session| session.stage().read_sync_attribute("/World/Local", "fence").unwrap().is_some());
        let property = session.snapshot_root_property("/World/Local", "x").unwrap();
        assert!(property.contains("local metadata"));
        let value = session.stage().read_sync_attribute("/World/Local", "x").unwrap();
        if initially_authored { assert_eq!(value.unwrap().value.doubles, [1.0]); }
        else { assert!(value.is_none(), "rejected default was restored: {property}"); }
    }
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn join_skips_early_attribute_frames_but_rejects_unresolved_snapshot_targets() {
    let mut session = open();
    let (peer, client) = Peer::connect();
    peer.send.send(vec![attributes("/World/Plane", 6, vec![double_attribute("width", 99.0)]),
        plane(4.0), attributes("/World/Plane", 7, vec![double_attribute("width", 9.0)]), snapshot(7)]).unwrap();
    session.attach_collab(client);
    pump_until(&mut session, StudioSession::collab_is_synced);
    assert_eq!(session.stage().read_sync_attribute("/World/Plane", "width").unwrap().unwrap().value.doubles, [9.0]);

    let mut session = open();
    let (peer, client) = Peer::connect();
    peer.send.send(vec![attributes("/World/Missing", 7, vec![double_attribute("width", 9.0)]), snapshot(7)]).unwrap();
    session.attach_collab(client);
    let deadline = Instant::now() + Duration::from_secs(5);
    while session.tick().is_ok() {
        assert!(Instant::now() < deadline, "unresolved snapshot must fail");
        thread::sleep(Duration::from_millis(2));
    }
    assert!(!session.collab_is_synced());
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn snapshot_activation_makes_descendant_attributes_addressable_before_deactivation() {
    let mut session = open();
    let source = session.root_layer_snapshot().unwrap();
    let closing = source.rfind('}').unwrap();
    let source = format!("{}\n    def Xform \"Hidden\" (active = false) {{\n        def Cube \"Child\" {{\n        }}\n    }}\n{}",
        &source[..closing], &source[closing..]);
    session.restore_root_layer_snapshot(&source).unwrap();
    let (peer, client) = Peer::connect();
    let active = |value| frame(0, base_message::Payload::SetPrimActiveBroadcast(proto::SetPrimActiveBroadcast {
        prim_path: "/World/Hidden".into(), active: value,
    }));
    peer.send.send(vec![active(true), attributes("/World/Hidden/Child", 0, vec![double_attribute("size", 3.0)]),
        active(false), snapshot(0)]).unwrap();
    session.attach_collab(client);
    pump_until(&mut session, StudioSession::collab_is_synced);
    assert!(session.stage().prim_at_path("/World/Hidden/Child").is_none());
    assert_eq!(session.stage().read_sync_attribute("/World/Hidden/Child", "size").unwrap().unwrap().value.doubles, [3.0]);
}
