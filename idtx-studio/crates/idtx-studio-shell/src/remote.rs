//! Remote server integration: the download-URL helper and the file/session
//! browser listing fetchers.
//!
//! [`remote_download_url`] composes authenticated download URLs (always
//! available; unit-tested). The `ensure_*_listing` methods populate the remote
//! file/session browsers and are gated on `remote` / `remote + authoring`.

#[cfg(feature = "remote")]
use idtx_editor_ui::remote_browser::RemoteBrowserState;

use crate::app::ApplicationState;

#[cfg(feature = "remote")]
pub(crate) type RemoteCompletion = Box<dyn FnOnce(&mut ApplicationState) + Send>;

#[cfg(feature = "remote")]
fn login_for(cfg: &idtx_remote::ServerConfig, token: Option<String>) -> Result<String, String> {
    if let Some(token) = token { return Ok(token); }
    idtx_auth::Origin::from_url(&cfg.url).map_err(|error| error.to_string())?;
    let password = idtx_remote::resolve_password(&cfg.password_hint).map_err(|error| error.to_string())?;
    idtx_remote::login(&cfg.url, &cfg.username, &password).map_err(|error| error.to_string())
}

/// Editor browsing explicitly permits public assets. LAN/loopback access and
/// the asset cap come from project settings and are replaced on project changes.
#[cfg(feature = "usd-resolver-http")]
pub(crate) fn configure_asset_network(network: &idtx_project::NetworkProjectConfig) {
    let cap = if network.max_asset_mb == 0 {
        idtx_runtime_manifest::DEFAULT_MAX_ASSET_MB
    } else {
        network.max_asset_mb
    };
    idtx_usd_resolver_http::set_network_policy(idtx_usd_resolver_http::NetworkPolicy::public_internet()
        .with_private_hosts(network.private_hosts.clone()).with_max_response_mb(cap));
}

impl ApplicationState {
    /// One background shell request at a time: repeated clicks cannot accumulate
    /// workers or completion payloads. Configuration changes invalidate replies.
    #[cfg(feature = "remote")]
    pub(crate) fn start_remote_job(&mut self, work: impl FnOnce() -> RemoteCompletion + Send + 'static) -> bool {
        if self.remote_job.is_some() {
            self.status_message = Some("A server request is already in progress.".into());
            return false;
        }
        let generation = self.remote_generation;
        let (reply, receiver) = std::sync::mpsc::sync_channel(1);
        let context = self.egui_ctx.clone();
        let spawn = std::thread::Builder::new().name("idtx-shell-network".into()).spawn(move || {
            let apply = work();
            let completion: RemoteCompletion = Box::new(move |app| {
                if app.remote_generation == generation { apply(app); }
            });
            let _ = reply.send(completion);
            context.request_repaint();
        });
        if let Err(error) = spawn { self.status_message = Some(error.to_string()); return false; }
        self.remote_job = Some(receiver);
        true
    }

    #[cfg(feature = "remote")]
    pub(crate) fn poll_remote_job(&mut self) {
        let Some(job) = &self.remote_job else { return; };
        match job.try_recv() {
            Ok(apply) => { self.remote_job = None; apply(self); }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.remote_job = None;
                self.status_message = Some("Server request worker stopped.".into());
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }
    /// Resolve a bearer token from the persisted server config and arm it on the
    /// HTTP USD resolver.
    ///
    /// This is the editor-shell counterpart to the standalone player's pre-open
    /// `prepare()` (`idtx-runtime-core/src/remote.rs`): the resolver's fetch
    /// callback reads the armed token on **every** download, so arming it up
    /// front authenticates not just an explicitly-opened remote stage but also
    /// any `http(s)://` sublayer / reference / payload reached from a *local*
    /// root layer — e.g. a Conductor project level that references remote
    /// payloads. Without this, opening such a level fails because the fetch runs
    /// unauthenticated.
    ///
    /// Idempotent and lazy: when a token is already held it only (re-)arms the
    /// resolver; otherwise it resolves the keychain password and performs the
    /// `POST /auth/login` once, caching the result in `self.bearer_token`. A
    /// no-op when no server is configured.
    ///
    /// Returns the error string on a failed login so interactive callers can
    /// surface it; the startup caller treats any error as best-effort.
    #[cfg(feature = "remote")]
    pub(crate) fn ensure_bearer_token(&mut self) -> Result<(), String> {
        if self.bearer_token.is_none() {
            let Some(cfg) = self.server_config.clone() else {
                return Ok(());
            };
            idtx_auth::Origin::from_url(&cfg.url).map_err(|e| e.to_string())?;
            let token = idtx_remote::resolve_password(&cfg.password_hint)
                .map_err(|e| e.to_string())
                .and_then(|pw| {
                    idtx_remote::login(&cfg.url, &cfg.username, &pw).map_err(|e| e.to_string())
                })?;
            self.bearer_token = Some(token);
        }
        // Arm the shared auth bucket so remote fetches — including those
        // triggered on demand by a local root layer — are authenticated. The USD
        // HTTP resolver, OpenExec REST datasources and behavior `net` egress all
        // read this one `idtx_auth` store.
        self.arm_server_token()
    }

    /// The login credential belongs to the configured server, never to an
    /// arbitrary stage URL selected later by a browser or a USD reference.
    #[cfg(feature = "remote")]
    pub(crate) fn arm_server_token(&self) -> Result<(), String> {
        if let (Some(cfg), Some(token)) = (&self.server_config, &self.bearer_token) {
            let origin = idtx_auth::Origin::from_url(&cfg.url).map_err(|e| e.to_string())?;
            idtx_auth::set_bearer_token_for_origins(&[origin.to_string()], token.clone())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Ensure the remote browser window is open and its file listing is populated.
    ///
    /// Returns `false` when the caller should `continue` to the next action
    /// (e.g. when login fails and we've already set an error on the browser).
    #[cfg(feature = "remote")]
    pub(crate) fn ensure_remote_browser_listing(&mut self) -> bool {
        let browser = self
            .remote_browser
            .get_or_insert_with(RemoteBrowserState::new);
        browser.loading = true;
        browser.error = None;

        let cfg = self.server_config.clone();
        let token = self.bearer_token.clone();
        let started = self.start_remote_job(move || {
            let result = (|| {
                let cfg = cfg.ok_or("Configure the server first.")?;
                let token = login_for(&cfg, token)?;
                let entries = idtx_remote::list_files(&cfg.url, &token).map_err(|error| error.to_string())?;
                Ok::<_, String>((token, entries))
            })();
            Box::new(move |app| {
                match result {
                    Ok((token, entries)) => {
                        app.bearer_token = Some(token);
                        if let Err(error) = app.arm_server_token() { app.status_message = Some(error); }
                        if let Some(browser) = &mut app.remote_browser { browser.entries = Some(entries); browser.loading = false; }
                    }
                    Err(error) => if let Some(browser) = &mut app.remote_browser { browser.error = Some(error); browser.loading = false; },
                }
            })
        });
        if !started && let Some(browser) = &mut self.remote_browser { browser.loading = false; }
        started
    }

    /// Ensure the session browser window is open and its listing is populated.
    ///
    /// Shares the same lazy-login logic as [`ensure_remote_browser_listing`] but
    /// calls `idtx_collab::session::list_sessions` instead of `list_files`.
    ///
    /// Returns `false` when the caller should `continue` (login failed).
    #[cfg(all(feature = "remote", feature = "authoring"))]
    pub(crate) fn ensure_session_browser_listing(&mut self) -> bool {
        use idtx_editor_ui::remote_browser::{SessionBrowserState, SessionEntry};

        let browser = self
            .session_browser
            .get_or_insert_with(SessionBrowserState::new);
        browser.loading = true;
        browser.error = None;

        let cfg = self.server_config.clone();
        let token = self.bearer_token.clone();
        let started = self.start_remote_job(move || {
            let result = (|| {
                let cfg = cfg.ok_or("Configure the server first.")?;
                let token = login_for(&cfg, token)?;
                let items = idtx_collab::session::list_sessions(&cfg.url, &token).map_err(|error| error.to_string())?;
                    let entries: Vec<SessionEntry> = items
                        .into_iter()
                        .filter_map(|s| {
                            if s.mode == idtx_collab::session::SessionMode::SingleEdit.as_str() {
                                None
                            } else {
                                Some(SessionEntry {
                                    session_id: s.session_id,
                                    usd_file: s.usd_file,
                                    mode: s.mode,
                                    client_count: s.client_count,
                                    created_at: s.created_at,
                                    ws_url: s.ws_url,
                                })
                            }
                        })
                        .collect();
                Ok::<_, String>((token, entries))
            })();
            Box::new(move |app| {
                match result {
                    Ok((token, entries)) => {
                        app.bearer_token = Some(token);
                        if let Err(error) = app.arm_server_token() { app.status_message = Some(error); }
                        if let Some(browser) = &mut app.session_browser { browser.entries = Some(entries); browser.loading = false; }
                    }
                    Err(error) => if let Some(browser) = &mut app.session_browser { browser.error = Some(error); browser.loading = false; },
                }
            })
        });
        if !started && let Some(browser) = &mut self.session_browser { browser.loading = false; }
        started
    }
}

/// Composes the authenticated USD download URL for a server-relative file path.
///
/// The download endpoint is `<server>/api/v1/download/<path>` — distinct from
/// the `/api/v1/files` *listing* endpoint (which returns 404 for a file
/// subpath). The `IdtxHttpArResolver` fetches this URL verbatim, and relative
/// sub-layer/payload references resolve against its parent directory.
#[cfg(feature = "remote")]
pub(crate) fn remote_download_url(server_url: &str, filepath: &str) -> String {
    // The path may carry Windows `\` separators when the editor runs on
    // Windows; the download endpoint (and the server behind it) expect `/`.
    let filepath = filepath.replace('\\', "/");
    format!(
        "{}/api/v1/download/{}",
        server_url.trim_end_matches('/'),
        filepath.trim_start_matches('/')
    )
}

/// Recover the server-relative file path from a download URL produced by
/// [`remote_download_url`]. Inverse of that helper: strips the
/// `<server>/api/v1/download/` prefix. Returns `None` when `url` is not a
/// download URL for `server_url` (e.g. a local-file path). Used to identify the
/// currently-open remote stage's server path.
///
/// Gated on `authoring` as well as `remote`: its only callers are the
/// authoring-only "Create Child → Asset…" flow, so a `remote`-without-`authoring`
/// build (e.g. idtx-inspector) would otherwise see it as dead code.
#[cfg(all(feature = "remote", feature = "authoring"))]
pub(crate) fn server_filepath_from_download_url(server_url: &str, url: &str) -> Option<String> {
    let prefix = format!("{}/api/v1/download/", server_url.trim_end_matches('/'));
    url.replace('\\', "/")
        .strip_prefix(&prefix)
        .map(|rest| rest.trim_start_matches('/').to_owned())
}

/// Compute a POSIX relative path from the directory of `from_stage` to
/// `to_asset`, both server paths relative to the server root (forward-slash
/// grammar). Used to author a reference/payload path relative to the current
/// stage, so USD resolves it against the stage layer's directory.
///
/// Same-directory and subdirectory results are anchored with `./`: the server
/// rejects a bare `filename` as a *search-path-anchored* asset path, requiring
/// `/abs`, `./rel` or `../rel`.
///
/// `("demo/scene.usda", "assets/car.usda")` → `"../assets/car.usda"`;
/// `("demo/scene.usda", "demo/car.usda")` → `"./car.usda"`;
/// `("scene.usda", "assets/car.usda")` → `"./assets/car.usda"`.
#[cfg(all(feature = "remote", feature = "authoring"))]
pub(crate) fn server_relative_path(from_stage: &str, to_asset: &str) -> String {
    let norm = |p: &str| -> Vec<String> {
        p.replace('\\', "/")
            .trim_start_matches('/')
            .split('/')
            .filter(|c| !c.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let mut from_dir = norm(from_stage);
    from_dir.pop(); // drop the stage's own filename → its containing directory
    let to = norm(to_asset);

    // Longest common directory prefix.
    let common = from_dir
        .iter()
        .zip(to.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let ups = from_dir.len() - common;
    let mut parts: Vec<String> = vec!["..".to_owned(); ups];
    parts.extend(to.into_iter().skip(common));
    if parts.is_empty() {
        // `to_asset` *is* the stage's directory; degenerate but keep a valid path.
        ".".to_owned()
    } else if parts[0] == ".." {
        // Already `../…`-anchored.
        parts.join("/")
    } else {
        // Same-dir or subdir: anchor with `./` so the server does not treat it as
        // a search-path-anchored asset path.
        format!("./{}", parts.join("/"))
    }
}

#[cfg(all(test, feature = "remote"))]
mod tests {
    use super::remote_download_url;
    #[cfg(feature = "authoring")]
    use super::{server_filepath_from_download_url, server_relative_path};
    #[test]
    fn remote_download_url_uses_download_endpoint() {
        assert_eq!(
            remote_download_url("https://host.example", "basic-scene.usda"),
            "https://host.example/api/v1/download/basic-scene.usda"
        );
    }

    #[test]
    fn remote_download_url_normalizes_slashes() {
        // Trailing slash on server and leading slash on path must not double up.
        assert_eq!(
            remote_download_url("https://host.example/", "/sub/dir/scene.usdc"),
            "https://host.example/api/v1/download/sub/dir/scene.usdc"
        );
    }

    #[test]
    fn remote_download_url_rewrites_windows_separators() {
        // A subfolder path built on Windows must still address the forward-slash
        // download endpoint.
        assert_eq!(
            remote_download_url("https://host.example", "demo\\simple_primitives.usda"),
            "https://host.example/api/v1/download/demo/simple_primitives.usda"
        );
    }

    #[cfg(feature = "authoring")]
    #[test]
    fn server_filepath_from_download_url_strips_prefix() {
        assert_eq!(
            server_filepath_from_download_url(
                "https://host.example",
                "https://host.example/api/v1/download/demo/scene.usda"
            ),
            Some("demo/scene.usda".to_owned())
        );
        // Trailing slash on the server URL must not break the strip.
        assert_eq!(
            server_filepath_from_download_url(
                "https://host.example/",
                "https://host.example/api/v1/download/scene.usda"
            ),
            Some("scene.usda".to_owned())
        );
        // A non-download URL (e.g. a local path) yields None.
        assert_eq!(
            server_filepath_from_download_url("https://host.example", "C:/local/scene.usda"),
            None
        );
    }

    #[cfg(feature = "authoring")]
    #[test]
    fn server_relative_path_walks_up_and_down() {
        // Sibling directory: up one, down into assets.
        assert_eq!(
            server_relative_path("demo/scene.usda", "assets/car.usda"),
            "../assets/car.usda"
        );
        // Same directory: anchored with "./" (a bare filename is rejected as a
        // search-path-anchored asset by the server).
        assert_eq!(
            server_relative_path("demo/scene.usda", "demo/car.usda"),
            "./car.usda"
        );
        // Stage at the server root: the asset path is relative to it, "./"-anchored.
        assert_eq!(
            server_relative_path("scene.usda", "assets/car.usda"),
            "./assets/car.usda"
        );
        // Deeper nesting on both sides.
        assert_eq!(
            server_relative_path("a/b/scene.usda", "a/c/d/car.usda"),
            "../c/d/car.usda"
        );
        // Subdirectory of the stage's own directory is also "./"-anchored.
        assert_eq!(
            server_relative_path("demo/scene.usda", "demo/sub/car.usda"),
            "./sub/car.usda"
        );
        // Windows separators and leading slashes are normalized.
        assert_eq!(
            server_relative_path("demo\\scene.usda", "/assets/car.usda"),
            "../assets/car.usda"
        );
    }
}
