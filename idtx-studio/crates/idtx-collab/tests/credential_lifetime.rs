use std::{net::TcpListener, time::{Duration, Instant}};
use idtx_collab::client::CollabClient;

#[test]
#[allow(clippy::result_large_err)] // tungstenite's callback fixes this result type.
fn shared_socket_disconnects_on_logout_and_reconnect_requires_current_login() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let ws = origin.replace("http://", "ws://");
    idtx_auth::set_bearer_token_for_origins(std::slice::from_ref(&origin), "first").unwrap();
    let (connected, accepted) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        for expected in ["Bearer first", "Bearer second"] {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut socket = tungstenite::accept_hdr(stream, |request: &tungstenite::handshake::server::Request, response| {
                assert_eq!(request.headers().get("Authorization").unwrap(), expected);
                Ok(response)
            }).unwrap();
            connected.send(()).unwrap();
            while socket.read().is_ok() {}
        }
    });
    let mut client = CollabClient::connect_shared(&ws, "session", false).unwrap();
    accepted.recv_timeout(Duration::from_secs(3)).unwrap();
    idtx_auth::clear_origin(&origin).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while client.is_connected() { assert!(Instant::now() < deadline); std::thread::sleep(Duration::from_millis(2)); }
    assert!(client.reconnect().is_err());
    idtx_auth::set_bearer_token_for_origins(&[origin], "second").unwrap();
    client.reconnect().unwrap();
    accepted.recv_timeout(Duration::from_secs(3)).unwrap();
    client.close();
    worker.join().unwrap();
}
