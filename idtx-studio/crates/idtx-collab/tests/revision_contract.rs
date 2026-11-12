//! A real WebSocket boundary: receipt is not successful stage application.
use std::{net::TcpListener, sync::mpsc, thread, time::{Duration, Instant}};
use idtx_collab::{client::CollabClient, proto::{self, BaseMessage, base_message}};
use prost::Message as _;
use idtx_collab::client::{RemoteEdit, RemoteEditKind};

enum Command {
    Send(Vec<BaseMessage>),
    Read(mpsc::SyncSender<BaseMessage>),
    Stop,
}

struct Wire {
    client: Option<CollabClient>,
    commands: mpsc::Sender<Command>,
    server: Option<thread::JoinHandle<()>>,
}

impl Wire {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (commands, receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            while let Ok(command) = receiver.recv_timeout(Duration::from_secs(5)) {
                match command {
                    Command::Send(frames) => for frame in frames {
                        socket.send(tungstenite::Message::Binary(frame.encode_to_vec().into())).unwrap();
                    },
                    Command::Read(reply) => {
                        let frame = socket.read().unwrap();
                        let tungstenite::Message::Binary(bytes) = frame else { panic!("expected edit frame") };
                        let _ = reply.send(BaseMessage::decode(bytes).unwrap());
                    }
                    Command::Stop => break,
                }
            }
        });
        let client = CollabClient::join(&format!("ws://{address}"), "test", "").unwrap();
        Self { client: Some(client), commands, server: Some(server) }
    }

    fn client(&self) -> &CollabClient { self.client.as_ref().unwrap() }
    fn send(&self, frames: Vec<BaseMessage>) { self.commands.send(Command::Send(frames)).unwrap(); }
    fn next(&self) -> RemoteEdit {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(edit) = self.client().next_inbound() { return edit; }
            assert!(Instant::now() < deadline, "no inbound envelope");
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn read(&self) -> BaseMessage {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.commands.send(Command::Read(sender)).unwrap();
        receiver.recv_timeout(Duration::from_secs(4)).unwrap()
    }
    fn synced(&self, seq: u64) {
        self.send(vec![frame(seq, base_message::Payload::SnapshotComplete(proto::SnapshotComplete {}))]);
        let barrier = self.next();
        assert!(!self.client().is_synced());
        assert!(self.client().acknowledge_inbound(&barrier));
        assert!(self.client().is_synced());
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(client) = self.client.take() { client.close(); }
        if let Some(server) = self.server.take() { let _ = server.join(); }
    }
}

fn frame(seq: u64, payload: base_message::Payload) -> BaseMessage {
    BaseMessage { session_id: Some("test".into()), payload: Some(payload), server_seq: seq, request_id: 0 }
}

fn active(seq: u64, path: &str) -> BaseMessage {
    frame(seq, base_message::Payload::SetPrimActiveBroadcast(proto::SetPrimActiveBroadcast { prim_path: path.into(), active: true }))
}

#[test]
fn queued_edits_keep_their_authored_base_and_acks_follow_application() {
    let wire = Wire::new();
    wire.synced(7);
    wire.send(vec![active(8, "/World/Remote")]);
    let remote = wire.next();
    assert_eq!(remote.server_seq, 8);
    assert_eq!(wire.client().applied_seq(), 7);
    wire.client().send_transform("/World/Local".into(), [0.0; 16]);
    assert!(wire.client().send_create_prim("/World", "Cube"));
    assert!(wire.client().send_set_prim_active("/World/Local", false));
    let mut transform_id = 0;
    for _ in 0..3 {
        let sent = wire.read();
        assert_eq!(sent.server_seq, 7, "receipt of revision 8 cannot rebase local intent");
        if matches!(sent.payload, Some(base_message::Payload::XformUpdate(_))) { transform_id = sent.request_id; }
    }
    assert_ne!(transform_id, 0);
    let mut ack = frame(9, base_message::Payload::Ack(proto::Ack { ok: true, ..Default::default() }));
    ack.request_id = transform_id;
    wire.send(vec![ack]);
    assert_eq!(wire.client().applied_seq(), 7);
    assert!(wire.client().acknowledge_inbound(&remote));
    assert!(!wire.client().acknowledge_inbound(&remote), "no double confirmation");
    let ack = wire.next();
    assert!(matches!(ack.kind, RemoteEditKind::Acknowledged { request_id } if request_id == transform_id));
    assert_eq!(wire.client().applied_seq(), 8, "ack receipt does not apply it");
    assert!(wire.client().acknowledge_inbound(&ack));
    assert_eq!(wire.client().applied_seq(), 9);
}

#[test]
fn presnapshot_intent_is_retained_without_rebasing_it() {
    let wire = Wire::new();
    wire.client().send_transform("/World/Local".into(), [0.0; 16]);
    wire.synced(7);
    let sent = wire.read();
    assert_eq!(sent.server_seq, 0, "snapshot application cannot rewrite the authored base");
}

#[test]
fn operation_journal_keeps_rejected_and_unacknowledged_intent() {
    use idtx_collab::journal::Outcome;
    let wire = Wire::new();
    wire.synced(7);
    wire.client().send_transform("/World/Local".into(), [3.0; 16]);
    let request = wire.read();
    let mut rejected = frame(8, base_message::Payload::Ack(proto::Ack { ok: false, error: "stale".into(), ..Default::default() }));
    rejected.request_id = request.request_id;
    wire.send(vec![rejected, active(9, "/Fence")]);
    let _ = wire.next();
    let journal = wire.client().operation_journal();
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0].request, request);
    assert_eq!(journal[0].outcome, Outcome::Rejected { reason: "stale".into() });
    assert!(wire.client().send_create_prim("/World", "Plane"));
    let create = wire.read();
    wire.commands.send(Command::Stop).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while wire.client().is_connected() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let journal = wire.client().operation_journal();
    assert_eq!(journal[1].request, create);
    assert_eq!(journal[1].outcome, Outcome::Indeterminate);
}

#[test]
fn live_prefix_retry_does_not_unlock_sending_before_application() {
    let wire = Wire::new();
    wire.synced(7);
    wire.send(vec![active(8, "/World/A")]);
    let edit = wire.next();
    wire.client().fail_inbound();
    wire.client().retry_application();
    assert!(!wire.client().is_synced());
    assert!(wire.client().acknowledge_inbound(&edit));
    assert_eq!(wire.client().applied_seq(), 8);
    assert!(wire.client().is_synced());
}

#[test]
fn reconnect_archives_failed_prefix_and_preserves_unknown_create_without_resending() {
    use idtx_collab::journal::Outcome;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (requests_tx, requests) = mpsc::channel();
    let (stop_tx, stop) = mpsc::channel();
    let server = thread::spawn(move || {
        for seq in [7, 9] {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            socket.send(tungstenite::Message::Binary(frame(seq, base_message::Payload::SnapshotComplete(proto::SnapshotComplete {})).encode_to_vec().into())).unwrap();
            let tungstenite::Message::Binary(bytes) = socket.read().unwrap() else { panic!("expected request") };
            requests_tx.send(BaseMessage::decode(bytes).unwrap()).unwrap();
            if seq == 7 {
                socket.send(tungstenite::Message::Binary(vec![255].into())).unwrap();
                socket.send(tungstenite::Message::Binary(active(8, "/Later").encode_to_vec().into())).unwrap();
                let _ = socket.read(); // reconnect closes the old connection
            } else {
                stop.recv_timeout(Duration::from_secs(4)).unwrap();
            }
        }
    });
    let mut client = CollabClient::join(&format!("ws://{address}"), "test", "").unwrap();
    let next = |client: &CollabClient| {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(edit) = client.next_inbound() { break edit; }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
    };
    assert!(client.acknowledge_inbound(&next(&client)));
    assert!(client.send_create_prim("/World", "Plane"));
    let create = requests.recv_timeout(Duration::from_secs(3)).unwrap();
    let invalid = next(&client);
    assert!(matches!(invalid.kind, RemoteEditKind::Invalid { .. }));
    client.fail_inbound();
    client.send_transform("/World/Local".into(), [1.0; 16]);
    client.reconnect().unwrap();
    assert!(!client.is_synced());
    let barrier = next(&client); // reconnect and archival complete asynchronously
    assert!(client.recovery_inbound().iter().any(|edit| matches!(edit.kind, RemoteEditKind::Invalid { .. })));
    assert_eq!(client.operation_journal()[0].outcome, Outcome::Indeterminate);
    assert!(matches!(barrier.kind, RemoteEditKind::SnapshotComplete));
    assert!(client.acknowledge_inbound(&barrier));
    let sent = requests.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(matches!(sent.payload, Some(base_message::Payload::XformUpdate(_))), "unknown create must not be duplicated");
    assert!(sent.request_id > create.request_id);
    assert_eq!(sent.server_seq, 7, "resync cannot rebase retained local intent");
    stop_tx.send(()).unwrap();
    client.close();
    server.join().unwrap();
}

#[test]
fn snapshot_sequence_group_is_applied_only_at_its_barrier() {
    let wire = Wire::new();
    wire.send(vec![active(7, "/World/A"), active(7, "/World/B"),
        frame(7, base_message::Payload::SnapshotComplete(proto::SnapshotComplete {}))]);
    for path in ["/World/A", "/World/B"] {
        let edit = wire.next();
        assert_eq!(edit.prim_path, path);
        assert!(wire.client().acknowledge_inbound(&edit));
        assert_eq!(wire.client().applied_seq(), 0);
        assert!(!wire.client().is_synced());
    }
    let barrier = wire.next();
    assert!(wire.client().acknowledge_inbound(&barrier));
    assert_eq!(wire.client().applied_seq(), 7);
    assert!(wire.client().is_synced());
}

#[test]
fn queued_live_sequence_group_does_not_advance_halfway() {
    let wire = Wire::new();
    wire.synced(7);
    wire.send(vec![active(8, "/World/A"), active(8, "/World/B"),
        frame(0, base_message::Payload::Error(proto::ProtocolError { code: "receipt-fence".into(), message: String::new() }))]);
    let deadline = Instant::now() + Duration::from_secs(3);
    while wire.client().drain_events().is_empty() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(wire.client().acknowledge_inbound(&wire.next()));
    assert_eq!(wire.client().applied_seq(), 7, "the second frame is unapplied");
    assert!(wire.client().acknowledge_inbound(&wire.next()));
    assert_eq!(wire.client().applied_seq(), 8);
}

#[test]
fn rejected_or_unknown_ack_does_not_advance_applied_revision() {
    let wire = Wire::new();
    wire.synced(7);
    wire.client().send_transform("/World/Local".into(), [0.0; 16]);
    let request = wire.read();
    let mut rejected = frame(99, base_message::Payload::Ack(proto::Ack { ok: false, error: "stale".into(), ..Default::default() }));
    rejected.request_id = request.request_id;
    let mut unknown = frame(100, base_message::Payload::Ack(proto::Ack { ok: true, ..Default::default() }));
    unknown.request_id = request.request_id + 10;
    wire.send(vec![rejected, unknown, active(8, "/World/Marker")]);
    let marker = wire.next();
    assert_eq!(marker.prim_path, "/World/Marker");
    assert_eq!(wire.client().applied_seq(), 7);
    assert!(wire.client().acknowledge_inbound(&marker));
    assert_eq!(wire.client().applied_seq(), 8);
}

#[test]
fn failed_prefix_cannot_be_confirmed_or_hidden_by_a_snapshot() {
    let wire = Wire::new();
    wire.send(vec![active(7, "/World/Bad"),
        frame(7, base_message::Payload::SnapshotComplete(proto::SnapshotComplete {}))]);
    let failed = wire.next();
    wire.client().fail_inbound();
    assert!(!wire.client().acknowledge_inbound(&failed));
    assert!(wire.client().next_inbound().is_none());
    assert!(!wire.client().is_synced());
    assert_eq!(wire.client().applied_seq(), 0);
}

#[test]
fn receiving_snapshot_complete_does_not_mark_the_stage_synced() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, wait) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        for payload in [
            base_message::Payload::SnapshotComplete(proto::SnapshotComplete {}),
            base_message::Payload::SetPrimActiveBroadcast(proto::SetPrimActiveBroadcast { prim_path: "/World".into(), active: true }),
        ] {
            let frame = BaseMessage { session_id: Some("test".into()), payload: Some(payload), server_seq: 7, request_id: 0 };
            socket.send(tungstenite::Message::Binary(frame.encode_to_vec().into())).unwrap();
        }
        let _ = wait.recv_timeout(Duration::from_secs(5));
    });
    let client = CollabClient::join(&format!("ws://{address}"), "test", "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if client.next_inbound().is_some() { break; }
        assert!(Instant::now() < deadline, "worker did not receive frames");
        thread::sleep(Duration::from_millis(2));
    }
    let synced = client.is_synced();
    let _ = stop.send(());
    client.close();
    server.join().unwrap();
    assert!(!synced, "receipt alone cannot make a stage synced");
}
