//! End-to-end tests for the HTTP(S) USD asset resolver.
//!
//! The first test is the regression test for the plugin-discovery bug:
//! `OpenUSD` reads `PXR_PLUGINPATH_NAME` in an `ARCH_CONSTRUCTOR` at
//! library-load time (before `main()`), so setting that env var from
//! `install()` at runtime is too late. `install()` must instead register the
//! plugin directory via `PlugRegistry::RegisterPlugins()` so that
//! `IdtxHttpArResolver` is associated with its `plugInfo.json` before `Ar`
//! enumerates URI resolvers on first stage open. Without the fix, opening an
//! `http://` stage fails with "Failed to find plugin for `IdtxHttpArResolver`".
//!
//! The force-refresh test covers the "Redownload" path: a normal open
//! serves the cache, while an open with force-refresh armed re-fetches every
//! asset — including a referenced sublayer — overwriting the cache.
//! Security regressions cover malicious Windows URL paths, an outside sentinel,
//! and relative dependencies when URL anchors carry queries/fragments.
//!
//! Require the `OpenUSD` runtime, so they are `#[ignore]`d by default. Run with:
//!
//! ```bash
//! export IDTX_USD_DIR=/path/to/openusd/install
//! export DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib
//! cargo test -p idtx-usd-resolver-http --test remote_open -- --ignored
//! ```

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use idtx_usd::StageService;

// Installation/cache selection and refresh are process-global.
static RESOLVER_TEST_LOCK: Mutex<()> = Mutex::new(());

const USDA: &str = r#"#usda 1.0
(
    defaultPrim = "root"
)

def Xform "root"
{
    def Sphere "ball"
    {
        double radius = 1.0
    }
}
"#;

/// Serves `USDA` at `/basic.usda` from an ephemeral localhost port until the
/// returned sender is dropped. Returns the bound port.
fn spawn_server() -> (u16, mpsc::Sender<()>) {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind local http server");
    let port = server.server_addr().to_ip().expect("ip addr").port();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    thread::spawn(move || {
        loop {
            // Non-blocking-ish poll so the thread notices the stop signal.
            match server.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(Some(request)) => {
                    let response = tiny_http::Response::new(
                        200.into(),
                        vec![
                            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/plain"[..])
                                .unwrap(),
                        ],
                        Cursor::new(USDA.as_bytes().to_vec()),
                        Some(USDA.len()),
                        None,
                    );
                    let _ = request.respond(response);
                }
                Ok(None) => {
                    if stop_rx.try_recv().is_ok() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    (port, stop_tx)
}

#[test]
#[ignore = "requires OpenUSD runtime (set IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn opens_stage_over_http_via_resolver() {
    let _lock = RESOLVER_TEST_LOCK.lock().unwrap();
    allow_fixture_network();
    let cache = tempfile::tempdir().expect("temp cache dir");
    idtx_usd_resolver_http::install(cache.path()).expect("install resolver");

    let (port, _stop) = spawn_server();
    let url = format!("http://127.0.0.1:{port}/basic.usda");

    let stage = StageService::open_topology_only(&url)
        .expect("open remote stage — resolver plugin must be discovered");

    // Pseudo-root + /root + /root/ball == at least three topology records.
    let prim_count = stage.topology().len();
    assert!(
        prim_count >= 3,
        "expected at least 3 prims from the remote stage, got {prim_count}"
    );

    // The downloaded asset must have landed in the cache directory.
    let cached: Vec<_> = walk(cache.path());
    assert!(
        cached.iter().any(|p| std::fs::read_to_string(p).ok().as_deref() == Some(USDA)),
        "expected the asset to be cached under {}; found {:?}",
        cache.path().display(),
        cached
    );
}

/// Serves a mutable set of paths (path → body). Returns the port, a handle to
/// swap bodies mid-flight, and a stop signal. Used to verify that a second open
/// serves the cache while a force-refresh open re-fetches the fresh body.
fn spawn_mutable_server(routes: Arc<Mutex<HashMap<String, String>>>) -> (u16, mpsc::Sender<()>) {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind local http server");
    let port = server.server_addr().to_ip().expect("ip addr").port();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    thread::spawn(move || {
        loop {
            match server.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(Some(request)) => {
                    let path = request.url().to_owned();
                    let body = routes
                        .lock()
                        .unwrap()
                        .get(&path)
                        .cloned()
                        .unwrap_or_default();
                    let response = tiny_http::Response::new(
                        if body.is_empty() { 404 } else { 200 }.into(),
                        vec![
                            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/plain"[..])
                                .unwrap(),
                        ],
                        Cursor::new(body.clone().into_bytes()),
                        Some(body.len()),
                        None,
                    );
                    let _ = request.respond(response);
                }
                Ok(None) => {
                    if stop_rx.try_recv().is_ok() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    (port, stop_tx)
}

#[test]
#[ignore = "requires OpenUSD runtime (set IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn force_refresh_redownloads_root_and_sublayer() {
    let _lock = RESOLVER_TEST_LOCK.lock().unwrap();
    allow_fixture_network();
    let cache = tempfile::tempdir().expect("temp cache dir");
    idtx_usd_resolver_http::install(cache.path()).expect("install resolver");

    let routes = Arc::new(Mutex::new(HashMap::new()));
    let (port, _stop) = spawn_mutable_server(routes.clone());
    let base = format!("http://127.0.0.1:{port}");
    let root_url = format!("{base}/root.usda");

    // v1: root layer defines /root and sublayers child.usda; the child adds a
    // Sphere `ball` under root → the composed stage has a `ball` prim, no `box`.
    let root_v1 = "#usda 1.0\n(\n    subLayers = [@child.usda@]\n)\n\ndef Xform \"root\"\n{\n}\n".to_owned();
    let child_v1 = r#"#usda 1.0

over "root"
{
    def Sphere "ball"
    {
        double radius = 1.0
    }
}
"#;
    {
        let mut r = routes.lock().unwrap();
        r.insert("/root.usda".into(), root_v1.clone());
        r.insert("/child.usda".into(), child_v1.into());
    }

    // First open: downloads root + child, composes `ball`.
    {
        let stage = StageService::open_topology_only(&root_url).expect("open v1");
        let names = prim_names(&stage);
        assert!(
            names.iter().any(|n| n == "ball"),
            "v1 must have ball: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "box"),
            "v1 must not have box: {names:?}"
        );
    }

    // Change the referenced sublayer on the server: the child now defines a
    // Cube `box` instead of the Sphere `ball`. The root layer is unchanged.
    let child_v2 = r#"#usda 1.0

over "root"
{
    def Cube "box"
    {
        double size = 2.0
    }
}
"#;
    {
        let mut r = routes.lock().unwrap();
        r.insert("/child.usda".into(), child_v2.into());
    }

    // Normal reopen (no force-refresh): the resolver serves the cached child,
    // so the composed stage is unchanged — still `ball`, still no `box`.
    {
        let stage = StageService::open_topology_only(&root_url).expect("reopen cached");
        let names = prim_names(&stage);
        assert!(
            names.iter().any(|n| n == "ball"),
            "cached reopen keeps ball: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "box"),
            "cached reopen has no box: {names:?}"
        );
    }

    // Force-refresh reopen: the resolver re-fetches root AND the referenced
    // sublayer, overwriting the cache, so the stage now reflects `box`.
    idtx_usd_resolver_http::set_force_refresh(true);
    let names = {
        let stage = StageService::open_topology_only(&root_url).expect("force-refresh reopen");
        prim_names(&stage)
    };
    idtx_usd_resolver_http::set_force_refresh(false);

    assert!(
        names.iter().any(|n| n == "box"),
        "force-refresh must re-fetch the sublayer and pick up box: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "ball"),
        "force-refresh sublayer no longer defines ball: {names:?}"
    );
}

fn prim_names(stage: &StageService) -> Vec<String> {
    stage.topology().iter().map(|r| r.name.clone()).collect()
}

fn allow_fixture_network() {
    idtx_usd_resolver_http::set_network_policy(
        idtx_usd_resolver_http::NetworkPolicy::new(true, vec!["127.0.0.1".into()])
            .with_private_hosts(vec!["127.0.0.1".into()]),
    );
}

#[test]
#[ignore = "requires OpenUSD runtime (set IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn private_assets_are_isolated_across_logout_accounts_refresh_and_projects() {
    let _lock = RESOLVER_TEST_LOCK.lock().unwrap();
    allow_fixture_network();
    idtx_auth::clear();
    let cache = tempfile::tempdir().unwrap();
    idtx_usd_resolver_http::install(cache.path()).unwrap();
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", server.server_addr().to_ip().unwrap());
    let url = format!("{origin}/private.bin");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let worker = thread::spawn(move || loop {
        if stop_rx.try_recv().is_ok() { break; }
        if let Some(request) = server.recv_timeout(std::time::Duration::from_millis(20)).unwrap() {
            let token = request.headers().iter().find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.to_string()).unwrap_or_default();
            observed.lock().unwrap().push(token.clone());
            let (status, body) = match token.as_str() {
                "Bearer A" => (200, "PRIVATE_A"),
                "Bearer B" => (200, "PRIVATE_B"),
                "Bearer refreshed" => (200, "PRIVATE_REFRESHED"),
                _ => (401, ""),
            };
            request.respond(tiny_http::Response::from_string(body).with_status_code(status)).unwrap();
        }
    });
    let stage_path = cache.path().join("reader.usda");
    std::fs::write(&stage_path, USDA).unwrap();
    let stage = StageService::open_topology_only(&stage_path).unwrap();
    let login = |token| idtx_auth::set_bearer_token_for_origins(&[origin.clone()], token).unwrap();
    login("A");
    assert_eq!(stage.read_asset(&url).unwrap(), b"PRIVATE_A");
    assert_eq!(stage.read_asset(&url).unwrap(), b"PRIVATE_A");
    assert_eq!(requests.lock().unwrap().len(), 1);
    idtx_auth::clear();
    assert!(stage.read_asset(&url).is_err(), "logged-out access must contact the server, not reuse A");
    login("B");
    assert_eq!(stage.read_asset(&url).unwrap(), b"PRIVATE_B");
    assert_eq!(requests.lock().unwrap().len(), 3);
    idtx_usd_resolver_http::set_force_refresh(true);
    let refreshed = stage.read_asset(&url);
    idtx_usd_resolver_http::set_force_refresh(false);
    assert_eq!(refreshed.unwrap(), b"PRIVATE_B");
    assert_eq!(requests.lock().unwrap().len(), 4);
    login("expired");
    idtx_auth::set_refresh_handler(Some(Arc::new(|_| Some("refreshed".into()))));
    assert_eq!(stage.read_asset(&url).unwrap(), b"PRIVATE_REFRESHED");
    assert_eq!(stage.read_asset(&url).unwrap(), b"PRIVATE_REFRESHED");
    assert_eq!(requests.lock().unwrap().len(), 6);
    idtx_auth::set_refresh_handler(None);
    let project_b = tempfile::tempdir().unwrap();
    idtx_usd_resolver_http::set_cache_dir(project_b.path()).unwrap();
    assert_eq!(stage.read_asset(&url).unwrap(), b"PRIVATE_REFRESHED");
    idtx_usd_resolver_http::set_cache_dir(cache.path()).unwrap();
    assert_eq!(stage.read_asset(&url).unwrap(), b"PRIVATE_REFRESHED");
    assert_eq!(requests.lock().unwrap().len(), 8);
    idtx_auth::clear();
    login("A");
    assert_eq!(stage.read_asset(&url).unwrap(), b"PRIVATE_A");
    assert_eq!(requests.lock().unwrap().len(), 9, "re-login must not resurrect an old generation");
    idtx_auth::clear();
    stop_tx.send(()).unwrap();
    worker.join().unwrap();
}

/// Replicates the visionOS/device bundle layout: the resolver dylib is already
/// load-resident (linked in here via the crate dep, as it is via the forced
/// `@rpath` load on device), and the plugInfo registered has an **emptied
/// `LibraryPath`** (so `PlugRegistry` must not `dlopen` a second copy). This is
/// exactly what the packager stages into `Frameworks/idtx-http-resolver/`.
///
/// Verifies that an `http://` payload is still routed to the resolver and fetched
/// under this layout — i.e. the emptied-`LibraryPath` registration actually works
/// for a URI resolver (the on-device "no remote assets" failure would reproduce
/// here as an open error / empty cache if it did not).
#[test]
#[ignore = "requires OpenUSD runtime (set IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn opens_http_stage_with_emptied_library_path_pluginfo() {
    let _lock = RESOLVER_TEST_LOCK.lock().unwrap();
    allow_fixture_network();
    let dir = tempfile::tempdir().expect("temp dir");
    let plugin_dir = dir.path().join("idtx-http-resolver");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    // A plugInfo declaring the http/https URI schemes with an EMPTY LibraryPath
    // (code already resident). No dylib file is placed beside it.
    std::fs::write(
        plugin_dir.join("plugInfo.json"),
        r#"{
  "Plugins": [{
    "Info": { "Types": { "IdtxHttpArResolver": {
      "bases": ["ArResolver"], "uriSchemes": ["http", "https"], "implementsContexts": false
    } } },
    "Type": "library",
    "Name": "idtxHttpResolver",
    "LibraryPath": "",
    "Root": "."
  }]
}
"#,
    )
    .unwrap();

    let cache = dir.path().join("cache");
    idtx_usd_resolver_http::install_with_plugin_dir(&cache, &plugin_dir)
        .expect("install resolver with emptied-LibraryPath plugInfo");

    let (port, _stop) = spawn_server();
    let url = format!("http://127.0.0.1:{port}/basic.usda");
    let stage = StageService::open_topology_only(&url)
        .expect("open remote stage — emptied-LibraryPath resolver must be discovered");

    assert!(
        stage.topology().len() >= 3,
        "emptied-LibraryPath resolver must resolve the remote stage"
    );
    let cached = walk(&cache);
    assert!(
        cached.iter().any(|p| std::fs::read_to_string(p).ok().as_deref() == Some(USDA)),
        "asset must be fetched by the resident (emptied-LibraryPath) resolver; cache={cached:?}"
    );
}

/// A relative reference containing "." / ".." segments must resolve to the
/// same identifier — and therefore the same cache-folder hash — as opening the
/// collapsed URL directly. Before normalization these produced two distinct
/// URL hashes and two cached copies of one remote asset; the resolver now
/// canonicalises the path via RFC 3986 `remove_dot_segments` in
/// `_CreateIdentifier`, so both map to a single cache entry. The server also
/// only knows the collapsed route, so a failure to normalize would 404.
#[test]
#[ignore = "requires OpenUSD runtime (set IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn dot_segments_collapse_to_single_cache_entry() {
    let _lock = RESOLVER_TEST_LOCK.lock().unwrap();
    allow_fixture_network();
    let cache = tempfile::tempdir().expect("temp cache dir");
    idtx_usd_resolver_http::install(cache.path()).expect("install resolver");

    let routes = Arc::new(Mutex::new(HashMap::new()));
    let (port, _stop) = spawn_mutable_server(routes.clone());
    let base = format!("http://127.0.0.1:{port}");

    // The child asset lives only at the collapsed path "/a/d/child.usda".
    let child = r#"#usda 1.0

over "root"
{
    def Sphere "ball"
    {
        double radius = 1.0
    }
}
"#;
    // The root, at "/a/b/root.usda", pulls the child in through a sublayer whose
    // path still carries "." / ".." segments: "c/../../d/child.usda" resolves
    // (relative to "/a/b/") to "/a/b/c/../../d/child.usda" → "/a/d/child.usda".
    let root = "#usda 1.0\n(\n    subLayers = [@c/../../d/child.usda@]\n)\n\ndef Xform \"root\"\n{\n}\n".to_owned();
    {
        let mut r = routes.lock().unwrap();
        r.insert("/a/b/root.usda".into(), root);
        r.insert("/a/d/child.usda".into(), child.into());
    }

    // Open via the relative reference, then open the collapsed URL directly.
    {
        let stage = StageService::open_topology_only(format!("{base}/a/b/root.usda"))
            .expect("open root that references child via dot segments");
        let names = prim_names(&stage);
        assert!(
            names.iter().any(|n| n == "ball"),
            "child must compose in: {names:?}"
        );
    }
    {
        let _ = StageService::open_topology_only(format!("{base}/a/d/child.usda"))
            .expect("open the collapsed child URL directly");
    }

    // Exactly one cached copy of the child — both opens shared one URL hash.
    let child_copies: Vec<_> = walk(cache.path())
        .into_iter()
        .filter(|p| std::fs::read_to_string(p).ok().as_deref() == Some(child))
        .collect();
    assert_eq!(
        child_copies.len(),
        1,
        "dot-segment reference and direct open must share one cache entry, found: {child_copies:?}"
    );
}

#[test]
#[ignore = "requires OpenUSD runtime (set IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn relative_dependencies_ignore_anchor_query_and_fragment() {
    let _lock = RESOLVER_TEST_LOCK.lock().unwrap();
    allow_fixture_network();
    let cache = tempfile::tempdir().unwrap();
    idtx_usd_resolver_http::install(cache.path()).unwrap();
    let routes = Arc::new(Mutex::new(HashMap::new()));
    let (port, _stop) = spawn_mutable_server(routes.clone());
    routes.lock().unwrap().insert("/root.usda?download=/elsewhere/file".into(),
        "#usda 1.0\n( subLayers = [@child.usda@] )\ndef Xform \"root\" {}\n".into());
    routes.lock().unwrap().insert("/child.usda".into(),
        "#usda 1.0\nover \"root\"\n{\n    def Sphere \"ball\" {}\n}\n".into());
    for fragment in ["first", "second/other.usda"] {
        let url = format!("http://127.0.0.1:{port}/root.usda?download=/elsewhere/file#{fragment}");
        let stage = StageService::open_topology_only(url).unwrap();
        assert!(prim_names(&stage).iter().any(|name| name == "ball"));
    }
    assert_eq!(walk(cache.path()).len(), 2, "fragments must share the root cache entry");
}

#[test]
#[ignore = "requires OpenUSD runtime (set IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn fetch_and_force_refresh_cannot_touch_an_outside_sentinel() {
    let _lock = RESOLVER_TEST_LOCK.lock().unwrap();
    allow_fixture_network();
    let scratch = tempfile::tempdir().unwrap();
    let cache = scratch.path().join("cache");
    let sentinel = scratch.path().join("proof.usda");
    std::fs::write(&sentinel, "outside sentinel").unwrap();
    idtx_usd_resolver_http::install(&cache).unwrap();
    let (port, _stop) = spawn_server();
    let base = format!("http://127.0.0.1:{port}");
    let drive_path = if cfg!(windows) {
        sentinel.to_string_lossy().replace('/', "\\")
    } else {
        r"C:\Users\reviewer\proof.usda".to_owned()
    };
    let attacks = [drive_path, r"..\proof.usda".into(), r"\\?\C:\proof.usda".into(),
        r"\\server\share\proof.usda".into(), "proof.usda:stream".into(), "..%5cproof.usda".into()];
    for force in [false, true] {
        idtx_usd_resolver_http::set_force_refresh(force);
        for path in &attacks {
            let _ = StageService::open_topology_only(format!("{base}/{path}"));
            assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "outside sentinel");
            assert!(walk(&cache).is_empty(), "unsafe URL must not be cached: {path}");
        }
        // Exercise a real successful fetch, not just URL rejection.
        let stage = StageService::open_topology_only(format!("{base}/basic.usda")).unwrap();
        assert!(stage.topology().len() >= 3);
        drop(stage);
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "outside sentinel");
        // Clear only our cache files so the next rejected URL can be checked.
        for path in walk(&cache) { std::fs::remove_file(path).unwrap(); }
    }
    idtx_usd_resolver_http::set_force_refresh(false);
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}
