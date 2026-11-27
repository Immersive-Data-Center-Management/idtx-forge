use std::sync::OnceLock;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;
#[cfg(feature = "metrics")]
use tracing::info;

use crate::error::RemoteError;

// ── shared HTTP client ────────────────────────────────────────────────────────

static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();

fn client() -> &'static Client {
    HTTP_CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(concat!("idtx-studio/", env!("CARGO_PKG_VERSION")))
            // Login bodies and explicit bearer credentials stay at the selected
            // server origin, including on 307/308 responses.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client init")
    })
}

// ── response shapes ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct LoginResponse {
    access_token: String,
    // Retained so the server's full OAuth-style token response deserializes
    // cleanly; only `access_token` is consumed today (see `login`). Kept for
    // when refresh/expiry handling lands.
    #[allow(dead_code)]
    expires_in: Option<u64>,
    #[allow(dead_code)]
    refresh_token: Option<String>,
    #[allow(dead_code)]
    scope: Option<String>,
    #[allow(dead_code)]
    token_type: Option<String>,
}

/// A single entry from the server's flat file listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEntry {
    /// Server-relative path used for download/session endpoints.
    pub filepath: String,
    pub filename: String,
    pub directory: String,
    pub size: u64,
    pub modified: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileListResponse {
    pub files: Vec<RemoteEntry>,
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn bounded_body(response: reqwest::blocking::Response) -> Result<Vec<u8>, RemoteError> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    response.take(1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|error| RemoteError::Parse(error.to_string()))?;
    if bytes.len() > 1024 * 1024 { return Err(RemoteError::Parse("response exceeds 1 MiB".into())); }
    Ok(bytes)
}

fn check_response(
    resp: reqwest::blocking::Response,
) -> Result<reqwest::blocking::Response, RemoteError> {
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(RemoteError::Unauthorized);
    }
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = String::from_utf8_lossy(&bounded_body(resp)?).into_owned();
        return Err(RemoteError::Server { status, body });
    }
    Ok(resp)
}

// ── public API ────────────────────────────────────────────────────────────────

/// Verifies the server is reachable. Expects `GET {url}/api/v1/health` → 2xx.
pub fn health_check(url: &str) -> Result<(), RemoteError> {
    #[cfg(feature = "metrics")]
    let t = std::time::Instant::now();

    let endpoint = format!("{}/api/v1/health", url.trim_end_matches('/'));
    let resp = client().get(&endpoint).timeout(std::time::Duration::from_secs(30)).send()?;
    check_response(resp)?;

    #[cfg(feature = "metrics")]
    info!(ms = t.elapsed().as_millis(), "health_check");

    debug!(url, "health check OK");
    Ok(())
}

/// Authenticates and returns a bearer token.
/// Expects `POST {url}/api/v1/login` with JSON body `{username, password}`.
pub fn login(url: &str, username: &str, password: &str) -> Result<String, RemoteError> {
    #[cfg(feature = "metrics")]
    let t = std::time::Instant::now();

    let endpoint = format!("{}/api/v1/auth/login", url.trim_end_matches('/'));
    let body = serde_json::json!({ "username": username, "password": password });
    let resp = client().post(&endpoint).timeout(std::time::Duration::from_secs(30)).json(&body).send()?;
    let resp = check_response(resp)?;

    let lr: LoginResponse = serde_json::from_slice(&bounded_body(resp)?).map_err(|e| RemoteError::Parse(e.to_string()))?;

    #[cfg(feature = "metrics")]
    info!(ms = t.elapsed().as_millis(), "login");

    debug!(url, username, "login OK");
    Ok(lr.access_token)
}

/// Returns the flat file listing from the server.
/// Expects `GET {url}/api/v1/files` with `Authorization: Bearer <token>`.
pub fn list_files(url: &str, token: &str) -> Result<Vec<RemoteEntry>, RemoteError> {
    #[cfg(feature = "metrics")]
    let t = std::time::Instant::now();

    let endpoint = format!("{}/api/v1/files", url.trim_end_matches('/'));
    let resp = client().get(&endpoint).timeout(std::time::Duration::from_secs(30)).bearer_auth(token).send()?;
    let resp = check_response(resp)?;

    let response: FileListResponse = serde_json::from_slice(&bounded_body(resp)?).map_err(|e| RemoteError::Parse(e.to_string()))?;

    #[cfg(feature = "metrics")]
    info!(
        count = response.files.len(),
        ms = t.elapsed().as_millis(),
        "list_files"
    );

    debug!(url, count = response.files.len(), "file listing received");
    Ok(response.files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_browser_rejects_oversized_responses() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let worker = std::thread::spawn(move || {
            let request = server.recv_timeout(std::time::Duration::from_secs(3)).unwrap().unwrap();
            let _ = request.respond(tiny_http::Response::from_data(vec![b' '; 1024 * 1024 + 1]));
        });
        assert!(list_files(&base, "").unwrap_err().to_string().contains("exceeds 1 MiB"));
        worker.join().unwrap();
    }

    #[test]
    fn login_body_does_not_follow_a_redirect_to_another_origin() {
        let target = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let source = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", source.server_addr().to_ip().unwrap());
        let location = format!("http://{}/stolen", target.server_addr().to_ip().unwrap());
        let worker = std::thread::spawn(move || {
            let request = source.recv_timeout(std::time::Duration::from_secs(3)).unwrap().expect("login request");
            request.respond(tiny_http::Response::empty(307)
                .with_header(tiny_http::Header::from_bytes("Location", location).unwrap())).unwrap();
        });
        assert!(login(&base, "synthetic-user", "synthetic-password").is_err());
        worker.join().unwrap();
        assert!(target.recv_timeout(std::time::Duration::from_millis(100)).unwrap().is_none());
    }
}
