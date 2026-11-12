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
//! The second test covers force-refresh (the "Redownload" path): a normal open
//! serves the cache, while an open with force-refresh armed re-fetches every
//! asset — including a referenced sublayer — overwriting the cache.
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
        cached.iter().any(|p| p.ends_with("basic.usda")),
        "expected basic.usda to be cached under {}; found {:?}",
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
    allow_fixture_network();
    let cache = tempfile::tempdir().expect("temp cache dir");
    idtx_usd_resolver_http::install(cache.path()).expect("install resolver");

    let routes = Arc::new(Mutex::new(HashMap::new()));
    let (port, _stop) = spawn_mutable_server(routes.clone());
    let base = format!("http://127.0.0.1:{port}");
    let root_url = format!("{base}/root.usda");

    // v1: root layer defines /root and sublayers child.usda; the child adds a
    // Sphere `ball` under root → the composed stage has a `ball` prim, no `box`.
    let root_v1 = format!(
        "#usda 1.0\n(\n    subLayers = [@{base}/child.usda@]\n)\n\ndef Xform \"root\"\n{{\n}}\n"
    );
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
        cached.iter().any(|p| p.ends_with("basic.usda")),
        "asset must be fetched by the resident (emptied-LibraryPath) resolver; cache={cached:?}"
    );
}

/// A relative reference containing "." / ".." segments must resolve to the
/// same identifier — and therefore the same cache-folder hash — as opening the
/// collapsed URL directly. Before normalization these produced two distinct
/// FNV hashes and two cached copies of one remote asset; the resolver now
/// canonicalises the path via RFC 3986 `remove_dot_segments` in
/// `_CreateIdentifier`, so both map to a single cache entry. The server also
/// only knows the collapsed route, so a failure to normalize would 404.
#[test]
#[ignore = "requires OpenUSD runtime (set IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn dot_segments_collapse_to_single_cache_entry() {
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
    let root = format!(
        "#usda 1.0\n(\n    subLayers = [@{base}/a/b/c/../../d/child.usda@]\n)\n\ndef Xform \"root\"\n{{\n}}\n"
    );
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

    // Exactly one cached copy of child.usda — both opens shared one hash folder.
    let child_copies: Vec<_> = walk(cache.path())
        .into_iter()
        .filter(|p| p.ends_with("child.usda"))
        .collect();
    assert_eq!(
        child_copies.len(),
        1,
        "dot-segment reference and direct open must share one cache entry, found: {child_copies:?}"
    );
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
