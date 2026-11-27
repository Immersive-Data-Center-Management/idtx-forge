//! Shared single-stage command line for Inspector and Poser.

use std::path::PathBuf;

/// Optional stage argument, flattened into each product's clap command.
#[derive(Debug, clap::Args)]
pub struct StageArgs {
    /// Existing USD file, local file:// URI, or configured HTTP(S) URL.
    #[arg(value_name = "USD_FILE_OR_URL")]
    pub stage: Option<PathBuf>,
}

impl StageArgs {
    /// Validate before launching the desktop shell. Use the application's own
    /// feature flag: workspace feature unification can enable remote in the
    /// shell even when this particular binary was built without it.
    pub fn into_initial_stage(self, remote_enabled: bool) -> Result<Option<PathBuf>, String> {
        self.stage
            .map(|path| parse_stage_argument(path, remote_enabled))
            .transpose()
    }
}

fn uri_scheme(value: &str) -> Option<&str> {
    let (scheme, _) = value.split_once(':')?;
    // Drive-absolute and drive-relative Windows paths are filesystem paths.
    if scheme.len() <= 1 || !scheme.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }
    scheme
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
        .then_some(scheme)
}

fn parse_stage_argument(path: PathBuf, remote_enabled: bool) -> Result<PathBuf, String> {
    let Some((value, scheme)) = path
        .to_str()
        .and_then(|s| uri_scheme(s).map(|scheme| (s, scheme)))
    else {
        return existing_file(path);
    };
    let is_file = scheme.eq_ignore_ascii_case("file");
    if !is_file && !remote_enabled {
        return Err("Remote URLs require a build with the `remote` feature enabled. Use a local USD file or install/rebuild this app with --features remote.".into());
    }
    let url = url::Url::parse(value).map_err(|_| "Invalid USD URL/URI.".to_owned())?;
    match url.scheme() {
        "file" => {
            if url.host_str().is_some_and(|host| host != "localhost") {
                return Err("Remote file:// authorities are not supported; use an HTTP(S) URL with a remote-enabled build.".into());
            }
            if url.query().is_some() || url.fragment().is_some() {
                return Err("A local file:// URI cannot have a query or fragment.".into());
            }
            let local = url
                .to_file_path()
                .map_err(|()| "Invalid local file:// URI.".to_owned())?;
            existing_file(local)
        }
        "http" | "https" => {
            if !value[scheme.len()..].starts_with("://") {
                return Err("A remote USD URL must start with http:// or https://.".into());
            }
            idtx_auth::Origin::from_url(url.as_str())
                .map_err(|_| "A remote USD URL must have a host and cannot contain a username or password; configure login data in the app.".to_owned())?;
            if url.fragment().is_some() {
                return Err("A remote USD URL cannot contain a fragment.".into());
            }
            Ok(PathBuf::from(url.as_str()))
        }
        _ => Err(format!(
            "Unsupported USD URI scheme '{}'; use a local file, file:// URI, or HTTP(S) URL.",
            url.scheme()
        )),
    }
}

fn existing_file(path: PathBuf) -> Result<PathBuf, String> {
    let metadata = std::fs::metadata(&path)
        .map_err(|error| format!("Cannot open USD file '{}': {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("USD path '{}' is not a file.", path.display()));
    }
    Ok(path)
}

/// Preserve the URL separately from the pseudo-path passed to OpenUSD, so the
/// session is tagged remote and Poser never offers an in-place filesystem save.
pub(crate) struct InitialStage {
    pub(crate) path: PathBuf,
    pub(crate) remote_url: Option<String>,
    #[cfg(feature = "remote")]
    pub(crate) remote: Option<RemoteStartup>,
}

impl InitialStage {
    pub(crate) fn prepare(path: PathBuf, app: &str) -> Result<Self, String> {
        let path = parse_stage_argument(path, cfg!(feature = "remote"))?;
        let remote_url = path
            .to_str()
            .filter(|value| matches!(uri_scheme(value), Some("http" | "https")))
            .map(str::to_owned);
        #[cfg(feature = "remote")]
        let remote = remote_url
            .as_deref()
            .map(|url| prepare_remote(url, app))
            .transpose()?;
        #[cfg(not(feature = "remote"))]
        let _ = app;
        Ok(Self {
            path,
            remote_url,
            #[cfg(feature = "remote")]
            remote,
        })
    }
}

#[cfg(feature = "remote")]
pub(crate) struct RemoteStartup {
    pub(crate) servers: Vec<idtx_remote::ServerConfig>,
    pub(crate) active_server: usize,
    pub(crate) origin: String,
    pub(crate) token: String,
}

#[cfg(feature = "remote")]
fn prepare_remote(url: &str, app: &str) -> Result<RemoteStartup, String> {
    let result = (|| {
        let servers = idtx_remote::load_configs()
            .map_err(|error| format!("Cannot read the server configuration: {error}"))?;
        let remote = authenticate_remote(
            url,
            servers,
            |hint| {
                idtx_remote::resolve_password(hint).map_err(|_| {
                    "The saved password is unavailable; save it again in Configure….".to_owned()
                })
            },
            |cfg, password| {
                idtx_remote::login(&cfg.url, &cfg.username, password)
                    .map_err(|error| format!("Server login failed: {error}"))
            },
        )?;
        idtx_auth::set_bearer_token_for_origins(
            std::slice::from_ref(&remote.origin),
            remote.token.clone(),
        )
        .map_err(|error| format!("Server login returned an unusable credential: {error}"))?;
        Ok(remote)
    })();
    result.map_err(|error: String| remote_setup_error(app, &error))
}

#[cfg(feature = "remote")]
fn remote_setup_error(app: &str, reason: &str) -> String {
    format!(
        "{reason}\nLaunch {app} without a parameter, open the {app} menu > Configure… (settings), add the target server URL and a description with its username and password, then Test and Save before retrying."
    )
}

// Inject credential lookup/login so rejection paths can be tested without the
// user's configuration, keychain, or a live server. No network call happens
// until the exact origin and the required login data have been checked.
#[cfg(feature = "remote")]
fn authenticate_remote(
    url: &str,
    servers: Vec<idtx_remote::ServerConfig>,
    resolve_password: impl FnOnce(&str) -> Result<String, String>,
    login: impl FnOnce(&idtx_remote::ServerConfig, &str) -> Result<String, String>,
) -> Result<RemoteStartup, String> {
    let origin = idtx_auth::Origin::from_url(url).map_err(|error| error.to_string())?;
    let active_server = servers
        .iter()
        .position(|cfg| {
            idtx_auth::Origin::from_url(&cfg.url).is_ok_and(|configured| configured == origin)
        })
        .ok_or_else(|| {
            format!("Remote origin {origin} is not allowed: no matching server is configured.")
        })?;
    let cfg = &servers[active_server];
    if cfg.username.trim().is_empty() || cfg.password_hint.is_empty() {
        return Err(format!(
            "Login data for {origin} is incomplete; a username and saved password are required."
        ));
    }
    let password = resolve_password(&cfg.password_hint)?;
    if password.is_empty() {
        return Err(format!("The saved password for {origin} is empty."));
    }
    let token = login(cfg, &password)?;
    Ok(RemoteStartup {
        servers,
        active_server,
        origin: origin.to_string(),
        token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Parser, error::ErrorKind};

    #[derive(Debug, Parser)]
    #[command(version = "1.0")]
    struct TestCli {
        #[command(flatten)]
        stage: StageArgs,
    }

    #[test]
    fn clap_accepts_no_stage_one_stage_and_end_of_options() {
        for app in ["idtx-inspector", "idtx-poser"] {
            assert!(
                TestCli::try_parse_from([app])
                    .unwrap()
                    .stage
                    .stage
                    .is_none()
            );
            let cli = TestCli::try_parse_from([app, "models/scene with spaces.usda"]).unwrap();
            assert_eq!(
                cli.stage.stage,
                Some(PathBuf::from("models/scene with spaces.usda"))
            );
            let cli = TestCli::try_parse_from([app, "--", "-scene.usda"]).unwrap();
            assert_eq!(cli.stage.stage, Some(PathBuf::from("-scene.usda")));
        }
    }

    #[test]
    fn clap_handles_help_version_and_rejects_extra_arguments() {
        for (args, expected) in [
            (vec!["--help"], ErrorKind::DisplayHelp),
            (vec!["--version"], ErrorKind::DisplayVersion),
            (vec!["--unknown"], ErrorKind::UnknownArgument),
            (vec!["a.usd", "b.usd"], ErrorKind::UnknownArgument),
        ] {
            let error = TestCli::try_parse_from(std::iter::once("app").chain(args)).unwrap_err();
            assert_eq!(error.kind(), expected);
        }
    }

    #[test]
    fn local_files_and_percent_encoded_file_uris_open_without_remote() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scene with spaces ü.usda");
        std::fs::write(&path, "#usda 1.0\n").unwrap();
        assert_eq!(parse_stage_argument(path.clone(), false).unwrap(), path);
        let url = url::Url::from_file_path(&path).unwrap();
        assert!(url.as_str().contains("%20"));
        assert_eq!(
            parse_stage_argument(url.as_str().into(), false).unwrap(),
            path
        );
        let stage = InitialStage::prepare(path.clone(), "IDTX Inspector").unwrap();
        assert_eq!(stage.path, path);
        assert!(stage.remote_url.is_none());
    }

    #[test]
    fn missing_files_and_directories_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            parse_stage_argument(dir.path().join("missing.usda"), false)
                .unwrap_err()
                .contains("Cannot open USD file")
        );
        assert!(
            parse_stage_argument(dir.path().to_owned(), false)
                .unwrap_err()
                .contains("not a file")
        );
    }

    #[test]
    fn windows_and_relative_paths_are_not_uri_schemes() {
        for path in [
            r"C:\models\scene.usda",
            "C:/models/scene.usda",
            "C:scene.usda",
            r"\\server\share\scene.usda",
            "./name:scene.usda",
        ] {
            assert_eq!(uri_scheme(path), None, "{path}");
        }
        assert_eq!(uri_scheme("HTTPS://host/scene.usda"), Some("HTTPS"));
    }

    // macOS (APFS/HFS+) enforces valid UTF-8 filenames and rejects non-UTF-8
    // byte sequences at the syscall level with EILSEQ, so such a file can never
    // exist there. Other unices (Linux, BSD) treat filenames as raw bytes.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn clap_preserves_non_utf8_filesystem_paths() {
        use std::os::unix::ffi::OsStringExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join(std::ffi::OsString::from_vec(b"scene-\xff.usda".to_vec()));
        std::fs::write(&path, "#usda 1.0\n").unwrap();
        let cli = TestCli::try_parse_from([
            std::ffi::OsString::from("app"),
            path.clone().into_os_string(),
        ])
        .unwrap();
        assert_eq!(cli.stage.into_initial_stage(false).unwrap(), Some(path));
    }

    #[test]
    fn remote_requires_the_application_feature() {
        // Passing false models an app without remote even if the shell's
        // remote feature is enabled by another workspace package.
        for url in [
            "https://host/scene.usda",
            "HTTP://host/scene.usda",
            "s3://bucket/scene.usda",
        ] {
            let error = parse_stage_argument(url.into(), false).unwrap_err();
            assert!(error.contains("--features remote"), "{error}");
        }
    }

    #[test]
    fn http_urls_keep_escaped_paths_and_queries() {
        let path = parse_stage_argument(
            "HTTPS://ASSETS.EXAMPLE:443/api/stage%20one.usda?revision=2".into(),
            true,
        )
        .unwrap();
        assert_eq!(
            path.to_str(),
            Some("https://assets.example/api/stage%20one.usda?revision=2")
        );
    }

    #[test]
    fn unsupported_or_ambiguous_urls_are_rejected() {
        for value in [
            "https://",
            "https:/host/scene.usda",
            "https://host/scene.usda#fragment",
            "https://user:secret@host/scene.usda",
            "s3://bucket/scene.usda",
            "file://server/share/scene.usda",
            "file:///tmp/scene.usda?query=1",
        ] {
            let error = parse_stage_argument(value.into(), true).unwrap_err();
            assert!(!error.contains("secret"), "credentials must not be echoed");
        }
    }

    #[cfg(feature = "remote")]
    mod remote {
        use super::*;
        use idtx_remote::ServerConfig;

        fn config(url: &str) -> ServerConfig {
            ServerConfig {
                description: "Test".into(),
                url: url.into(),
                username: "alice".into(),
                password_hint: "test-hint".into(),
            }
        }

        #[test]
        fn unconfigured_origins_never_read_credentials_or_login() {
            for configs in [vec![], vec![config("https://assets.example")]] {
                for target in [
                    "https://other.example/scene.usda",
                    "http://assets.example/scene.usda",
                    "https://assets.example:444/scene.usda",
                    "https://sub.assets.example/scene.usda",
                    "https://assets.example.attacker.test/scene.usda",
                ] {
                    let error = authenticate_remote(
                        target,
                        configs.clone(),
                        |_| panic!("unconfigured origin read credentials"),
                        |_, _| panic!("unconfigured origin logged in"),
                    )
                    .err()
                    .unwrap();
                    assert!(error.contains("not allowed"), "{error}");
                }
            }
        }

        #[test]
        fn selects_the_matching_server_and_its_login_data() {
            let prepared = authenticate_remote(
                "https://ASSETS.EXAMPLE:443/files/scene.usda",
                vec![
                    config("https://other.example"),
                    config("https://assets.example/service"),
                ],
                |hint| {
                    assert_eq!(hint, "test-hint");
                    Ok("saved-password".into())
                },
                |cfg, password| {
                    assert_eq!(cfg.url, "https://assets.example/service");
                    assert_eq!(cfg.username, "alice");
                    assert_eq!(password, "saved-password");
                    Ok("server-token".into())
                },
            )
            .unwrap();
            assert_eq!(prepared.active_server, 1);
            assert_eq!(prepared.origin, "https://assets.example");
            assert_eq!(prepared.token, "server-token");
        }

        #[test]
        fn incomplete_login_data_is_rejected_before_credential_lookup() {
            for (username, hint) in [("  ", "hint"), ("alice", "")] {
                let mut cfg = config("https://assets.example");
                cfg.username = username.into();
                cfg.password_hint = hint.into();
                let error = authenticate_remote(
                    "https://assets.example/scene.usda",
                    vec![cfg],
                    |_| panic!("incomplete login data"),
                    |_, _| panic!("incomplete login data"),
                )
                .err()
                .unwrap();
                assert!(error.contains("incomplete"));
            }
        }

        #[test]
        fn missing_or_empty_keychain_passwords_never_login() {
            for password in [Ok(String::new()), Err("keychain unavailable".into())] {
                assert!(
                    authenticate_remote(
                        "https://assets.example/scene.usda",
                        vec![config("https://assets.example")],
                        |_| password,
                        |_, _| panic!("unavailable password")
                    )
                    .is_err()
                );
            }
        }

        #[test]
        fn failed_login_is_fatal_and_guidance_explains_recovery() {
            let error = authenticate_remote(
                "https://assets.example/scene.usda",
                vec![config("https://assets.example")],
                |_| Ok("saved-password".into()),
                |_, _| Err("login rejected".into()),
            )
            .err()
            .unwrap();
            let message = remote_setup_error("IDTX Poser", &error);
            for part in [
                "login rejected",
                "IDTX Poser",
                "without a parameter",
                "Configure",
                "server URL",
                "username and password",
                "Test and Save",
            ] {
                assert!(message.contains(part), "{message}");
            }
            assert!(!message.contains("saved-password"));
        }
    }
}
