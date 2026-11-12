use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

const CAP: usize = 1024 * 1024;

fn policy() -> NetworkPolicy {
    NetworkPolicy::new(true, vec!["127.0.0.1".into()])
        .with_private_hosts(vec!["127.0.0.1".into()])
        .with_max_response_mb(1)
}

#[test]
fn unauthorized_asset_refreshes_once_with_the_new_shared_credential() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", server.server_addr().to_ip().unwrap());
    idtx_auth::set_bearer_token_for_origins(&[origin.clone()], "expired").unwrap();
    let expected = origin.clone();
    idtx_auth::set_refresh_handler(Some(std::sync::Arc::new(move |origin| { assert_eq!(origin, expected); Some("refreshed".into()) })));
    let worker = std::thread::spawn(move || {
        for (token, status, body) in [("Bearer expired", 401, ""), ("Bearer refreshed", 200, "asset")] {
            let request = server.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
            assert_eq!(request.headers().iter().find(|h| h.field.equiv("Authorization")).unwrap().value.as_str(), token);
            request.respond(tiny_http::Response::from_string(body).with_status_code(status)).unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(fetch_to_cache(&format!("{origin}/level.usda"), &dir.path().join("asset"), &policy(), Duration::from_secs(3)).unwrap(), 5);
    worker.join().unwrap();
    idtx_auth::set_refresh_handler(None);
    idtx_auth::clear_origin(&origin).unwrap();
}

/// Raw HTTP fixture controls framing, truncation and timing independently of an
/// HTTP server library's automatic Content-Length handling.
fn serve(parts: Vec<(Duration, Vec<u8>)>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/asset.usda?token=query-secret#fragment-secret", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return,
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        stream.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") && request.len() < 16 * 1024 {
            if stream.read_exact(&mut byte).is_err() { return; }
            request.push(byte[0]);
        }
        for (delay, bytes) in parts {
            if !delay.is_zero() { std::thread::sleep(delay); }
            if stream.write_all(&bytes).is_err() { break; }
        }
    });
    (url, worker)
}

fn immediate(bytes: impl Into<Vec<u8>>) -> (Duration, Vec<u8>) {
    (Duration::ZERO, bytes.into())
}

#[test]
fn exact_cap_download_atomically_replaces_existing_cache() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("asset.usda");
    std::fs::write(&dest, "old").unwrap();
    let (url, server) = serve(vec![
        immediate(format!("HTTP/1.1 200 OK\r\nContent-Length: {CAP}\r\n\r\n")),
        immediate(vec![b'x'; CAP]),
    ]);
    assert_eq!(fetch_to_cache(&url, &dest, &policy(), Duration::from_secs(2)).unwrap(), CAP as u64);
    server.join().unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), vec![b'x'; CAP]);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn oversized_and_truncated_bodies_never_replace_cache_or_leave_partial_files() {
    let cases = [
        vec![immediate(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", CAP + 1))],
        vec![immediate("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"),
            immediate(format!("{:x}\r\n", CAP + 1)), immediate(vec![b'x'; CAP + 1]), immediate("\r\n0\r\n\r\n")],
        vec![immediate("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n"), immediate(vec![b'x'; CAP + 1])],
        vec![immediate("HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort")],
    ];
    for (i, parts) in cases.into_iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("asset.usda");
        std::fs::write(&dest, "keep").unwrap();
        let (url, server) = serve(parts);
        let error = fetch_to_cache(&url, &dest, &policy(), Duration::from_secs(2)).unwrap_err();
        server.join().unwrap();
        if i < 3 { assert!(matches!(error, FetchError::TooLarge { .. }), "{error}"); }
        assert!(!error.to_string().contains("secret"));
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "keep");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

#[test]
fn redirects_never_contact_a_second_destination() {
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    target.set_nonblocking(true).unwrap();
    let location = format!("http://{}/stolen", target.local_addr().unwrap());
    let (url, server) = serve(vec![immediate(format!(
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n",
    ))]);
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(fetch_to_cache(&url, &dir.path().join("asset"), &policy(), Duration::from_secs(1)),
        Err(FetchError::HttpStatus(307))));
    server.join().unwrap();
    assert_eq!(target.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn stalled_headers_and_slow_drip_bodies_obey_one_total_deadline() {
    let cases = [
        vec![(Duration::from_millis(500), b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec())],
        std::iter::once(immediate("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"))
            .chain((0..12).map(|_| (Duration::from_millis(40), b"1\r\nx\r\n".to_vec())))
            .chain(std::iter::once(immediate("0\r\n\r\n"))).collect(),
    ];
    for parts in cases {
        let dir = tempfile::tempdir().unwrap();
        let (url, server) = serve(parts);
        let started = Instant::now();
        let result = fetch_to_cache(&url, &dir.path().join("asset"), &policy(), Duration::from_millis(120));
        let elapsed = started.elapsed();
        server.join().unwrap();
        assert!(result.is_err(), "slow input escaped the total request deadline");
        assert!(elapsed < Duration::from_millis(450), "fetch took {elapsed:?}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert!(!result.unwrap_err().to_string().contains("secret"));
    }
}

#[test]
fn failed_configuration_resets_policy_before_the_manifest_is_read() {
    let _lock = TEST_POLICY_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.toml");
    for contents in [None, Some("[network\nallow = true"), Some("version = 999\n[network]\nallow = true"),
        Some("[network]\nallow = true\nauth_origins = [\"bad\"]")] {
        if let Some(text) = contents { std::fs::write(&path, text).unwrap(); }
        set_network_policy(policy());
        let loaded = configure_network(
            || {
                assert!(network_policy().is_denied(), "previous config survived into parsing");
                idtx_runtime_manifest::RuntimeManifest::load(&path)
            },
            |manifest| manifest.network.asset_policy(),
        );
        assert!(loaded.is_err());
        assert!(network_policy().is_denied());
    }
    std::fs::write(&path, "[network]\nallow = true\nallowed_hosts = [\"127.0.0.1\"]\nprivate_hosts = [\"127.0.0.1\"]\n").unwrap();
    configure_network(|| idtx_runtime_manifest::RuntimeManifest::load(&path), |m| m.network.asset_policy()).unwrap();
    assert!(!network_policy().is_denied());
    set_network_policy(NetworkPolicy::denied());
}

#[test]
fn callback_diagnostics_redact_userinfo_query_and_fragment() {
    use std::sync::Arc;
    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let _lock = TEST_POLICY_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let captured = Capture(Arc::new(Mutex::new(Vec::new())));
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
        .with_max_level(tracing::Level::DEBUG).with_writer(move || writer.clone()).finish();
    let dir = tempfile::tempdir().unwrap();
    let dest = CString::new(dir.path().join("asset").to_str().unwrap()).unwrap();
    set_network_policy(policy());
    tracing::subscriber::with_default(subscriber, || {
        let (url, server) = serve(vec![immediate("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n")]);
        let url = CString::new(url).unwrap();
        // SAFETY: these C strings remain valid through the callback.
        assert!(!unsafe { rust_fetch(url.as_ptr(), dest.as_ptr()) });
        server.join().unwrap();
        let url = CString::new("http://user-secret:password-secret@127.0.0.1/path?token=query-secret#fragment-secret").unwrap();
        // SAFETY: both pointers refer to live, terminated C strings.
        assert!(!unsafe { rust_fetch(url.as_ptr(), dest.as_ptr()) });
    });
    set_network_policy(NetworkPolicy::denied());
    let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("resolver fetch failed"), "missing callback diagnostics");
    assert!(logs.contains("HTTP 500"));
    assert!(!logs.contains("secret"), "credential data in resolver diagnostics: {logs}");
}
