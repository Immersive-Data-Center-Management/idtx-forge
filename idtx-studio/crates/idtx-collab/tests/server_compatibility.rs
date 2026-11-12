//! Opt-in deployed-server contract. Operates on an explicitly supplied disposable
//! stage containing /World, and commits test edits to that file.
use std::time::{Duration, Instant};
use idtx_collab::{client::{CollabClient, RemoteEdit, RemoteEditKind}, session::*};

fn pump(clients: &[&CollabClient], seen: &mut [Vec<RemoteEdit>], ready: impl Fn(&[Vec<RemoteEdit>]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        for (client, edits) in clients.iter().zip(seen.iter_mut()) {
            while let Some(edit) = client.next_inbound() {
                assert!(!matches!(edit.kind, RemoteEditKind::Invalid { .. }), "invalid state-bearing server message");
                // This wire fixture validates and records state before confirming
                // delivery. Real USD application is covered by session regressions.
                edits.push(edit.clone());
                assert!(edits.len() <= 4096, "test stage snapshot is too large");
                assert!(client.acknowledge_inbound(&edit));
            }
        }
        if ready(seen) { return; }
        assert!(Instant::now() < deadline, "compatible-server condition timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "requires disposable IDTX_TEST_SERVER_* environment; commits remote edits"]
fn two_clients_preserve_new_fields_through_commit_reopen_and_reconnect() {
    let required = |key| std::env::var(key).unwrap_or_else(|_| panic!("{key} is required; this test never self-skips"));
    let base = required("IDTX_TEST_SERVER_URL");
    let token = required("IDTX_TEST_SERVER_TOKEN");
    let stage = required("IDTX_TEST_SERVER_STAGE");
    let reference = required("IDTX_TEST_SERVER_REFERENCE");
    let info = create_session(&base, &token, &stage, SessionMode::CollaborativeEdit).unwrap();
    assert_eq!(info.protocol, "protobuf-binary");
    let ws = ws_url_from_http(&base, &info.ws_url);
    let a = CollabClient::connect(&ws, &info.session_id, &token).unwrap();
    let b = CollabClient::join(&ws, &info.session_id, &token).unwrap();
    let mut seen = [Vec::new(), Vec::new()];
    pump(&[&a, &b], &mut seen, |_| a.is_synced() && b.is_synced());
    seen.iter_mut().for_each(Vec::clear);
    assert!(a.send_create_prim("/World", "Plane"));
    pump(&[&a, &b], &mut seen, |seen| seen.iter().all(|edits| edits.iter().any(|e| matches!(&e.kind, RemoteEditKind::CreatePrim { type_name, .. } if type_name == "Plane"))));
    let plane = seen[0].iter().find(|e| matches!(&e.kind, RemoteEditKind::CreatePrim { type_name, .. } if type_name == "Plane")).unwrap();
    let path = plane.prim_path.clone();
    assert!(matches!(&plane.kind, RemoteEditKind::CreatePrim { width: Some(w), length: Some(l), axis: Some(_), .. } if *w > 0.0 && *l > 0.0));
    let matrix = [1.0, 0.25, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 2.0, 3.0, 1.0];
    assert!(a.send_transform(path.clone(), matrix));
    pump(&[&a, &b], &mut seen, |seen| seen[1].iter().any(|e| e.prim_path == path && e.kind == RemoteEditKind::Matrix(matrix)));
    for payload in [false, true] {
        assert!(a.send_create_reference("/World", &reference, payload));
        pump(&[&a, &b], &mut seen, |seen| seen.iter().all(|edits| edits.iter().any(|e| matches!(&e.kind, RemoteEditKind::CreateReference { as_payload, reference_path, .. } if *as_payload == payload && reference_path == &reference))));
    }
    let commit = a.begin_commit().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        pump(&[&a, &b], &mut seen, |_| true);
        match commit.try_recv() {
            Ok(result) => { result.unwrap(); break; }
            Err(std::sync::mpsc::TryRecvError::Empty) => { assert!(Instant::now() < deadline); std::thread::sleep(Duration::from_millis(5)); }
            Err(error) => panic!("commit worker failed: {error}"),
        }
    }
    a.close(); b.close();
    delete_session(&base, &token, &info.session_id).unwrap();
    let reopened = create_session(&base, &token, &stage, SessionMode::CollaborativeEdit).unwrap();
    let ws = ws_url_from_http(&base, &reopened.ws_url);
    let mut reopened_client = CollabClient::join(&ws, &reopened.session_id, &token).unwrap();
    for reconnect in [false, true] {
        if reconnect { reopened_client.reconnect().unwrap(); }
        let mut seen = [Vec::new()];
        pump(&[&reopened_client], &mut seen, |_| reopened_client.is_synced());
        assert!(seen[0].iter().any(|e| e.prim_path == path && e.kind == RemoteEditKind::Matrix(matrix)), "committed affine transform was lost on reopen/reconnect");
    }
    reopened_client.close();
    delete_session(&base, &token, &reopened.session_id).unwrap();
}
