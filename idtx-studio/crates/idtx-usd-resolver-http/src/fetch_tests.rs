use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

const CAP: usize = 1024 * 1024;

fn fetch(url: &str, directory: &Path, timeout: Duration) -> Result<u64, FetchError> {
    fetch_to_cache(url, &Cache::open(directory)?, &policy(), timeout)
}

fn policy() -> NetworkPolicy {
    NetworkPolicy::new(true, vec!["127.0.0.1".into()])
        .with_private_hosts(vec!["127.0.0.1".into()])
        .with_max_response_mb(1)
}

#[test]
fn unauthorized_asset_refreshes_once_with_the_new_shared_credential() {
    let _lock = TEST_POLICY_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
    assert_eq!(fetch(&format!("{origin}/level.usda"), dir.path(), Duration::from_secs(3)).unwrap(), 5);
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
    let (url, server) = serve(vec![
        immediate(format!("HTTP/1.1 200 OK\r\nContent-Length: {CAP}\r\n\r\n")),
        immediate(vec![b'x'; CAP]),
    ]);
    let dest = dir.path().join(cache_leaf(&url).unwrap());
    std::fs::write(&dest, "old").unwrap();
    assert_eq!(fetch(&url, dir.path(), Duration::from_secs(2)).unwrap(), CAP as u64);
    server.join().unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), vec![b'x'; CAP]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn fetch_and_refresh_use_the_retained_cache_root() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache");
    let cache = Cache::open(&path).unwrap();
    match std::fs::rename(&path, dir.path().join("original")) {
        Ok(()) => {}
        // A sharing violation/access denial on Windows pins the root.
        Err(error) if cfg!(windows) && error.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(error) => panic!("rename cache: {error}"),
    }
    std::fs::create_dir(&path).unwrap();
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/asset.usda", server.server_addr().to_ip().unwrap());
    let leaf = cache_leaf(&url).unwrap();
    let sentinel = path.join(&leaf);
    std::fs::write(&sentinel, "outside sentinel").unwrap();
    let worker = std::thread::spawn(move || {
        for body in ["first", "refreshed"] {
            let request = server.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
            request.respond(tiny_http::Response::from_string(body)).unwrap();
        }
    });
    for expected in ["first", "refreshed"] {
        fetch_to_cache(&url, &cache, &policy(), Duration::from_secs(3)).unwrap();
        assert_eq!(cache.directory.read_to_string(&leaf).unwrap(), expected);
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "outside sentinel");
    }
    worker.join().unwrap();
}

#[cfg(any(unix, windows))]
#[test]
fn outside_symlink_is_not_read_and_refresh_does_not_follow_it() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("cache");
    let cache = Cache::open(&cache_path).unwrap();
    let sentinel = dir.path().join("sentinel");
    std::fs::write(&sentinel, "keep").unwrap();
    let (url, worker) = serve(vec![immediate("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nasset")]);
    let leaf = cache_leaf(&url).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&sentinel, cache_path.join(&leaf)).unwrap();
    #[cfg(windows)]
    if let Err(error) = std::os::windows::fs::symlink_file(&sentinel, cache_path.join(&leaf)) {
        // Windows requires Developer Mode or SeCreateSymbolicLinkPrivilege.
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            eprintln!("symlink fixture unavailable: {error}");
            worker.join().unwrap();
            return;
        }
        panic!("create symlink: {error}");
    }
    assert!(cache.read(&leaf).is_err());
    fetch_to_cache(&url, &cache, &policy(), Duration::from_secs(2)).unwrap();
    worker.join().unwrap();
    assert_eq!(cache.directory.read_to_string(&leaf).unwrap(), "asset");
    assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "keep");
}

#[test]
fn switching_project_caches_does_not_reuse_another_projects_entry() {
    let _lock = TEST_POLICY_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let a = idtx_remote::project_cache_dir_in(dir.path(), "Project A");
    let b = idtx_remote::project_cache_dir_in(dir.path(), "Project B");
    let url = "https://assets.example.com/scene.usda";
    let leaf = cache_leaf(url).unwrap();
    for (path, body) in [(&a, "alpha"), (&b, "beta")] {
        let cache = Cache::open(path).unwrap();
        cache.directory.write(&leaf, body).unwrap();
    }
    for (path, body) in [(&a, b"alpha".as_slice()), (&b, b"beta".as_slice())] {
        set_cache_dir(path).unwrap();
        let url = CString::new(url).unwrap();
        // SAFETY: URL is a live C string. We own the returned stream and close it.
        unsafe {
            let stream = rust_fetch(url.as_ptr(), false);
            assert!(!stream.is_null());
            let mut bytes = [0; 5];
            let count = libc::fread(bytes.as_mut_ptr().cast(), 1, bytes.len(), stream);
            libc::fclose(stream);
            assert_eq!(&bytes[..count], body);
        }
    }
    // Failure disables the old cache instead of silently sharing it.
    let blocked = dir.path().join("not-a-directory");
    std::fs::write(&blocked, "file").unwrap();
    assert!(set_cache_dir(&blocked).is_err());
    assert!(CACHE.lock().unwrap().is_none());
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
        let (url, server) = serve(parts);
        let dest = dir.path().join(cache_leaf(&url).unwrap());
        std::fs::write(&dest, "keep").unwrap();
        let error = fetch(&url, dir.path(), Duration::from_secs(2)).unwrap_err();
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
    assert!(matches!(fetch(&url, dir.path(), Duration::from_secs(1)),
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
        let result = fetch(&url, dir.path(), Duration::from_millis(120));
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
    set_cache_dir(dir.path()).unwrap();
    set_network_policy(policy());
    tracing::subscriber::with_default(subscriber, || {
        let (url, server) = serve(vec![immediate("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n")]);
        let url = CString::new(url).unwrap();
        // SAFETY: these C strings remain valid through the callback.
        assert!(unsafe { rust_fetch(url.as_ptr(), true) }.is_null());
        server.join().unwrap();
        let url = CString::new("http://user-secret:password-secret@127.0.0.1/path?token=query-secret#fragment-secret").unwrap();
        // SAFETY: the pointer refers to a live, terminated C string.
        assert!(unsafe { rust_fetch(url.as_ptr(), true) }.is_null());
    });
    set_network_policy(NetworkPolicy::denied());
    let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("resolver fetch failed"), "missing callback diagnostics");
    assert!(logs.contains("HTTP 500"));
    assert!(!logs.contains("secret"), "credential data in resolver diagnostics: {logs}");
}
