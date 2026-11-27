//! Commit must never turn receipt-only or pending work into a saved document.
use std::{io::{Read, Write}, net::TcpListener, sync::mpsc, thread, time::{Duration, Instant}};
use idtx_collab::client::CollabClient;
use idtx_collab::proto::{self, BaseMessage, base_message};
use prost::Message as _;

struct Server {
    messages: mpsc::Sender<BaseMessage>,
    requests: mpsc::Receiver<BaseMessage>,
    http: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    stop: mpsc::Sender<()>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl Server {
    fn new() -> (Self, CollabClient) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (messages, commands) = mpsc::channel::<BaseMessage>();
        let (request_tx, requests) = mpsc::channel();
        let (http_tx, http) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (stop, stopped) = mpsc::channel();
        let (http_listener_tx, http_listener) = mpsc::sync_channel(1);
        let ws = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_millis(10))).unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            http_listener_tx.send(listener).unwrap();
            loop {
                if let Ok(message) = commands.try_recv()
                    && socket.send(tungstenite::Message::Binary(message.encode_to_vec().into())).is_err() { break; }
                match socket.read() {
                    Ok(tungstenite::Message::Binary(data)) => { let _ = request_tx.send(BaseMessage::decode(data).unwrap()); }
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                    Err(_) => break,
                }
            }
        });
        let http_worker = thread::spawn(move || {
            let listener = http_listener.recv().unwrap();
            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while stopped.try_recv().is_err() && Instant::now() < deadline {
                if let Ok((mut stream, _)) = listener.accept() {
                    stream.set_nonblocking(false).unwrap();
                    stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                    read_headers(&mut stream);
                    http_tx.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(3)).unwrap();
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                }
                thread::sleep(Duration::from_millis(2));
            }
        });
        let client = CollabClient::join(&format!("ws://{address}"), "test", "").unwrap();
        (Self { messages, requests, http, release, stop, workers: vec![ws, http_worker] }, client)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.release.send(());
        let _ = self.stop.send(());
        for worker in self.workers.drain(..) { let _ = worker.join(); }
    }
}

fn message(seq: u64, id: u64, payload: base_message::Payload) -> BaseMessage {
    BaseMessage { server_seq: seq, request_id: id, session_id: Some("test".into()), payload: Some(payload) }
}

fn read_headers(stream: &mut std::net::TcpStream) {
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
        assert!(headers.len() < 16384);
    }
}

fn next(client: &CollabClient) -> idtx_collab::client::RemoteEdit {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(edit) = client.next_inbound() { return edit; }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn commit_waits_for_ack_and_application_and_defers_newer_edits() {
    let (server, client) = Server::new();
    server.messages.send(message(7, 0, base_message::Payload::SnapshotComplete(proto::SnapshotComplete {}))).unwrap();
    client.acknowledge_inbound(&next(&client));
    client.send_transform("/Before".into(), [1.0; 16]);
    let started = Instant::now();
    let commit = client.begin_commit().unwrap();
    assert!(started.elapsed() < Duration::from_millis(100));
    client.send_transform("/After".into(), [2.0; 16]);
    let request = server.requests.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(server.http.recv_timeout(Duration::from_millis(150)).is_err());
    server.messages.send(message(8, request.request_id, base_message::Payload::Ack(proto::Ack { ok: true, ..Default::default() }))).unwrap();
    let ack = next(&client);
    assert!(server.http.recv_timeout(Duration::from_millis(150)).is_err(), "receipt cannot release persistence");
    client.acknowledge_inbound(&ack);
    server.http.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(server.requests.recv_timeout(Duration::from_millis(150)).is_err(), "newer edit must wait behind HTTP");
    server.release.send(()).unwrap();
    assert_eq!(commit.recv_timeout(Duration::from_secs(3)).unwrap(), Ok(8));
    let after = server.requests.recv_timeout(Duration::from_secs(3)).unwrap();
    let Some(base_message::Payload::XformUpdate(after)) = after.payload else { panic!("expected deferred transform") };
    assert_eq!(after.prim_path, "/After");
    client.close();
}

#[test]
fn rejected_operations_block_commit_without_http() {
    let (server, client) = Server::new();
    server.messages.send(message(7, 0, base_message::Payload::SnapshotComplete(proto::SnapshotComplete {}))).unwrap();
    client.acknowledge_inbound(&next(&client));
    assert!(client.send_create_prim("/World", "Plane"));
    let commit = client.begin_commit().unwrap();
    let request = server.requests.recv_timeout(Duration::from_secs(3)).unwrap();
    server.messages.send(message(7, request.request_id, base_message::Payload::Ack(proto::Ack { ok: false, error: "stale".into(), ..Default::default() }))).unwrap();
    assert!(commit.recv_timeout(Duration::from_secs(3)).unwrap().is_err());
    assert!(server.http.try_recv().is_err());
    client.close();
}

#[test]
fn unsynced_commit_is_rejected_before_http() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (stop_tx, stop) = mpsc::channel();
    let (http_tx, http) = mpsc::channel();
    let worker = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _socket = tungstenite::accept(stream).unwrap();
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while stop.try_recv().is_err() && Instant::now() < deadline {
            if let Ok((mut stream, _)) = listener.accept() {
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                read_headers(&mut stream);
                http_tx.send(()).unwrap();
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
            thread::sleep(Duration::from_millis(2));
        }
    });
    let client = CollabClient::join(&format!("ws://{address}"), "test", "").unwrap();
    let result = client.commit();
    stop_tx.send(()).unwrap();
    worker.join().unwrap();
    assert!(result.is_err(), "an unsynchronized stage cannot be committed");
    assert!(http.try_recv().is_err(), "pre-sync commit must not reach HTTP");
    client.close();
}
