//! Real C++ resolver policy regression. Uses unique URLs and a fresh cache so a
//! cached layer cannot hide an unexpected network request.

use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
use std::time::Duration;

#[test]
#[ignore = "requires OpenUSD 26.11.1 runtime"]
fn resolver_defaults_to_deny_and_host_listing_does_not_grant_private_access() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let worker = std::thread::spawn(move || {
        while !stopping.load(Ordering::Relaxed) {
            if let Some(request) = server.recv_timeout(Duration::from_millis(20)).unwrap() {
                seen.fetch_add(1, Ordering::Relaxed);
                let _ = request.respond(tiny_http::Response::from_string(
                    "#usda 1.0\ndef Xform \"World\" {}\n",
                ));
            }
        }
    });
    let cache = tempfile::tempdir().unwrap();
    idtx_usd_resolver_http::install(cache.path()).unwrap();
    let unconfigured = idtx_usd::StageService::open_topology_only(format!("{base}/default.usda"));
    idtx_usd_resolver_http::set_allowed_hosts(&["127.0.0.1".into()]);
    let listed_private = idtx_usd::StageService::open_topology_only(format!("{base}/private.usda"));
    let before_grant = requests.load(Ordering::Relaxed);
    idtx_usd_resolver_http::set_network_policy(
        idtx_usd_resolver_http::NetworkPolicy::new(true, vec!["127.0.0.1".into()])
            .with_private_hosts(vec!["127.0.0.1".into()]),
    );
    let granted = idtx_usd::StageService::open_topology_only(format!("{base}/granted.usda"));
    stop.store(true, Ordering::Relaxed);
    worker.join().unwrap();
    assert!(unconfigured.is_err(), "unconfigured resolver fetched a remote layer");
    assert!(listed_private.is_err(), "host listing implicitly granted private access");
    assert_eq!(before_grant, 0);
    assert!(granted.is_ok());
    assert_eq!(requests.load(Ordering::Relaxed), 1);
}
