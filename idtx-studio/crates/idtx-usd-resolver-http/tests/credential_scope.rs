//! Credential isolation through the real C++ resolver and Rust fetch callback.

use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use std::time::Duration;

const USD: &str = "#usda 1.0\ndef Xform \"World\" {}\n";

#[test]
#[ignore = "requires OpenUSD 26.11.1 runtime"]
fn real_resolver_uses_only_authorized_origins() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let worker = std::thread::spawn(move || {
        while !stopping.load(Ordering::Relaxed) {
            if let Some(request) = server.recv_timeout(Duration::from_millis(20)).unwrap() {
                let header = request.headers().iter().find(|h| h.field.equiv("Authorization"))
                    .map(|h| h.value.as_str().to_owned());
                captured.lock().unwrap().push((request.url().to_owned(), header));
                request.respond(tiny_http::Response::from_string(USD)).unwrap();
            }
        }
    });
    let cache = tempfile::tempdir().unwrap();
    idtx_usd_resolver_http::set_network_policy(
        idtx_usd_resolver_http::NetworkPolicy::new(true, vec!["127.0.0.1".into()])
            .with_private_hosts(vec!["127.0.0.1".into()]),
    );
    idtx_usd_resolver_http::install(cache.path()).unwrap();
    idtx_auth::clear();
    // A legacy bare-host scope now means HTTPS/default port, not every scheme
    // and port on this machine. The pre-fix resolver leaks this token here.
    let _ = idtx_auth::set_bearer_token_for_hosts(&["127.0.0.1".into()], "wrong-origin");
    let first = idtx_usd::StageService::open_topology_only(format!("{base}/unscoped.usda")).unwrap();
    drop(first);
    let first_header = seen.lock().unwrap().iter().find(|(p, _)| p == "/unscoped.usda").unwrap().1.clone();

    let _ = idtx_auth::set_bearer_token_for_hosts(std::slice::from_ref(&base), "scoped");
    let second = idtx_usd::StageService::open_topology_only(format!("{base}/scoped.usda")).unwrap();
    drop(second);
    let second_header = seen.lock().unwrap().iter().find(|(p, _)| p == "/scoped.usda").unwrap().1.clone();

    idtx_auth::clear();
    let third = idtx_usd::StageService::open_topology_only(format!("{base}/logout.usda")).unwrap();
    drop(third);
    let third_header = seen.lock().unwrap().iter().find(|(p, _)| p == "/logout.usda").unwrap().1.clone();
    stop.store(true, Ordering::Relaxed);
    worker.join().unwrap();
    assert_eq!(first_header, None, "resolver attached a credential outside its origin");
    assert_eq!(second_header.as_deref(), Some("Bearer scoped"));
    assert_eq!(third_header, None, "logout must revoke the next lookup");
}
