use std::{net::TcpListener, sync::mpsc, thread, time::{Duration, Instant}};
use idtx_collab::client::CollabClient;

#[test]
fn session_http_rejects_oversized_success_and_error_bodies() {
    use std::io::{Read as _, Write as _};
    for status in ["200 OK", "500 Error"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let peer = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            stream.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut byte = [0];
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") { stream.read_exact(&mut byte).unwrap(); headers.push(byte[0]); }
            let body = vec![b' '; 1024 * 1024 + 1];
            write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            let _ = stream.write_all(&body);
        });
        let error = idtx_collab::session::list_sessions(&format!("http://{address}"), "").unwrap_err();
        assert!(error.to_string().contains("exceeds 1 MiB"), "{error}");
        peer.join().unwrap();
    }
}

#[test]
fn stalled_upgrade_never_blocks_connect_or_close() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (release, wait) = mpsc::channel();
    let peer = thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        let _ = wait.recv_timeout(Duration::from_millis(700));
    });
    let start = Instant::now();
    let result = CollabClient::join(&format!("ws://{address}"), "test", "");
    let elapsed = start.elapsed();
    if let Ok(client) = result {
        let close = Instant::now();
        client.close();
        assert!(close.elapsed() < Duration::from_millis(100));
    } else { panic!("connect must return an asynchronous handle"); }
    let _ = release.send(());
    peer.join().unwrap();
    assert!(elapsed < Duration::from_millis(100), "upgrade blocked for {elapsed:?}");
}

#[test]
fn inactive_consumer_backpressures_without_losing_the_ordered_prefix() {
    use idtx_collab::{proto::{self, BaseMessage, base_message}, client::RemoteEditKind};
    use prost::Message as _;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (release, wait) = mpsc::channel();
    let peer = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        for seq in 1..=1000 {
            let frame = BaseMessage { server_seq: seq, payload: Some(base_message::Payload::SetPrimActiveBroadcast(proto::SetPrimActiveBroadcast {
                prim_path: format!("/World/Prim{seq}"), active: true,
            })), ..Default::default() };
            socket.send(tungstenite::Message::Binary(frame.encode_to_vec().into())).unwrap();
        }
        let _ = wait.recv_timeout(Duration::from_secs(5));
    });
    let client = CollabClient::join(&format!("ws://{address}"), "test", "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.inbound_count() < 256 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    thread::sleep(Duration::from_millis(50));
    assert_eq!(client.inbound_count(), 256);
    for seq in 1..=1000 {
        let edit = loop {
            if let Some(edit) = client.next_inbound() { break edit; }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(edit.server_seq, seq);
        assert!(matches!(edit.kind, RemoteEditKind::SetActive { active: true }));
        assert!(client.acknowledge_inbound(&edit));
        assert!(client.inbound_count() <= 256);
    }
    let _ = release.send(());
    client.close();
    peer.join().unwrap();
}

#[test]
fn unsynced_intent_is_rejected_at_a_finite_limit() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (release, wait) = mpsc::channel();
    let (ready, upgraded) = mpsc::channel();
    let peer = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _socket = tungstenite::accept(stream).unwrap();
        ready.send(()).unwrap();
        let _ = wait.recv_timeout(Duration::from_secs(5));
    });
    let client = CollabClient::join(&format!("ws://{address}"), "test", "").unwrap();
    upgraded.recv_timeout(Duration::from_secs(3)).unwrap();
    let mut accepted = 0;
    for _ in 0..20_000 {
        if !client.send_create_prim("/World", "Cube") { break; }
        accepted += 1;
    }
    let _ = release.send(());
    client.close();
    peer.join().unwrap();
    assert!(accepted > 0 && accepted < 20_000, "unbounded accepted intent: {accepted}");
}

#[test]
fn single_prim_drag_coalesces_and_never_exhausts_the_queue_budget() {
    // Dragging one prim produces many transform samples that coalesce
    // last-writer-wins into a single pending edit. They must therefore be
    // charged once (per prim, per commit-cycle), so a long drag of a single
    // prim is never rejected with `local_queue_full`, even past the 4096 limit.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (release, wait) = mpsc::channel();
    let (ready, upgraded) = mpsc::channel();
    let peer = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _socket = tungstenite::accept(stream).unwrap();
        ready.send(()).unwrap();
        let _ = wait.recv_timeout(Duration::from_secs(5));
    });
    let client = CollabClient::join(&format!("ws://{address}"), "test", "").unwrap();
    upgraded.recv_timeout(Duration::from_secs(3)).unwrap();
    for sample in 0..20_000u32 {
        let mut matrix = [0.0f64; 16];
        matrix[12] = f64::from(sample); // vary the translation each sample
        assert!(
            client.send_transform("/World/DraggedCube".into(), matrix),
            "single-prim drag sample {sample} was rejected; coalesced samples must not accumulate charges",
        );
    }
    // Distinct prims are each real pending intent and must still be bounded.
    let mut distinct_accepted = 0;
    for prim in 0..20_000u32 {
        if !client.send_transform(format!("/World/Prim{prim}"), [0.0; 16]) { break; }
        distinct_accepted += 1;
    }
    let _ = release.send(());
    client.close();
    peer.join().unwrap();
    assert!(distinct_accepted < 20_000, "distinct-prim transform intent must stay bounded: {distinct_accepted}");
}

