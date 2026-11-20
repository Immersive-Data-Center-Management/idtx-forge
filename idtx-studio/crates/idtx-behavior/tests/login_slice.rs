#![cfg(feature = "net")]
use std::{collections::BTreeMap, time::{Duration, Instant}};
use bevy_ecs::world::World;
use idtx_behavior::{BehaviorHost, BehaviorManifest, Capabilities, InputRouter, Limits, PlayState, SessionEditOp, ecs, entity_to_wire};

#[test]
#[ignore = "compiles the shipped login to wasm32 and runs a local HTTP fixture"]
fn shipped_login_uses_entered_values_masks_password_and_requests_travel() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let temp = tempfile::tempdir().unwrap();
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", server.server_addr().to_ip().unwrap());
    let source = std::fs::read_to_string(root.join("tests/fixtures/behaviors/auth_login/auth_login.rs")).unwrap()
        .replace("https://core.example.local", &origin);
    let path = temp.path().join("auth_login.rs");
    std::fs::write(&path, source).unwrap();
    let artifact = idtx_behavior_build::build_behavior(&path, &idtx_behavior_build::BuildOptions::new(temp.path().join("out"), root.join("crates/idtx-behavior-sdk"))).unwrap();
    let (received, request_started) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        for attempt in 0..2 {
            let mut request = server.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            assert_eq!(body, r#"{"username":"ab","password":"c"}"#);
            if attempt == 1 { received.send(()).unwrap(); released.recv_timeout(Duration::from_secs(3)).unwrap(); }
            request.respond(tiny_http::Response::from_string(r#"{"access_token":"per-user-token"}"#)).unwrap();
        }
    });
    let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::default());
    host.set_network_policy(idtx_net_guard::NetworkPolicy::new(true, vec!["127.0.0.1".into()]).with_private_hosts(vec!["127.0.0.1".into()]));
    host.set_auth_scopes(idtx_auth::AuthScopes::parse(&[origin.clone()]).unwrap());
    host.register_module(&std::fs::read(artifact.wasm_path).unwrap(), BehaviorManifest {
        name: "login".into(), version: "1".into(), uuid: "login".into(), kind: idtx_behavior_abi::BehaviorKind::Runtime,
        ship: true, params: BTreeMap::new(), inputs: Vec::new(), capabilities: Capabilities::all(), memory_mb: None,
    }).unwrap();
    let mut world = World::new();
    let panel = entity_to_wire(world.spawn((ecs::Transform::default(), ecs::GlobalTransform::default())).id());
    host.attach(&mut world, panel, "login").unwrap();
    host.run_frame(&mut world, 0.016, PlayState::Playing); // on_start, no PlayStart event
    let mut saw_mask = false;
    for command in ["Append", "Next", "Append", "Password", "Next", "Append", "Login"] {
        assert!(host.post_widget_click(panel, command), "unregistered {command}");
        host.run_frame(&mut world, 0.016, PlayState::Playing);
        for edit in host.drain_session_edits() {
            if let SessionEditOp::SetWidgetValue { name, value } = edit.op {
                if name == "PasswordValue" { assert!(value.bytes().all(|b| b == b'*')); saw_mask |= value == "*"; }
            }
        }
    }
    assert!(saw_mask);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        host.run_frame(&mut world, 0.016, PlayState::Playing);
        let transitions = host.drain_level_transitions();
        if !transitions.is_empty() { assert_eq!(transitions[0].target, "remote"); break; }
        assert!(Instant::now() < deadline, "login callback did not request travel");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(idtx_auth::authorization_header_for(&origin).as_deref(), Some("Bearer per-user-token"));
    assert!(host.post_widget_click(panel, "Logout"));
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(idtx_auth::authorization_header_for(&origin), None);
    assert!(host.post_widget_click(panel, "Return"));
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(host.drain_level_transitions()[0].target, "login");
    for command in ["Append", "Login"] {
        assert!(host.post_widget_click(panel, command));
        host.run_frame(&mut world, 0.016, PlayState::Playing);
    }
    request_started.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(host.post_widget_click(panel, "Cancel"));
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    release.send(()).unwrap();
    worker.join().unwrap();
    for _ in 0..30 {
        host.run_frame(&mut world, 0.016, PlayState::Playing);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(host.drain_level_transitions().is_empty(), "cancelled login requested travel");
    assert_eq!(idtx_auth::authorization_header_for(&origin), None, "cancelled login restored credentials");
}
