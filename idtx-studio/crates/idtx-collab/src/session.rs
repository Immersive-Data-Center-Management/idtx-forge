use serde::Deserialize;

use crate::error::CollabError;

fn client() -> Result<reqwest::blocking::Client, CollabError> {
    Ok(reqwest::blocking::Client::builder().redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(5)).timeout(std::time::Duration::from_secs(30))
        .build()?)
}

fn body(response: reqwest::blocking::Response) -> Result<Vec<u8>, CollabError> {
    use std::io::Read as _;
    const LIMIT: u64 = 1024 * 1024;
    let mut bytes = Vec::new();
    response.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT { return Err(CollabError::Io(std::io::Error::other("collaboration response exceeds 1 MiB"))); }
    Ok(bytes)
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub ws_url: String,
    pub protocol: String,
    pub usd_file: String,
}

/// A session entry returned by `GET /api/v1/sessions`.
///
/// Contains the full server-side representation of a live session, including
/// peer count and creation timestamp, for display in the session browser.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionListItem {
    pub session_id: String,
    pub usd_file: String,
    pub mode: String,
    pub client_count: i64,
    pub created_at: i64,
    pub ws_url: String,
    #[serde(default)]
    pub protocol: String,
}

/// Wrapper for the `GET /api/v1/sessions` response envelope.
#[derive(Debug, Deserialize)]
struct SessionListResponse {
    sessions: Vec<SessionListItem>,
}

/// GET /api/v1/sessions — return all currently active sessions on the server.
pub fn list_sessions(base_url: &str, token: &str) -> Result<Vec<SessionListItem>, CollabError> {
    let url = format!("{}/api/v1/sessions", base_url.trim_end_matches('/'));
    let resp = client()?
        .get(&url)
        .timeout(std::time::Duration::from_secs(30))
        .bearer_auth(token)
        .send()?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(CollabError::Unauthorized);
    }
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body_text = String::from_utf8_lossy(&body(resp)?).into_owned();
        return Err(CollabError::Http {
            status,
            body: body_text,
        });
    }

    let envelope: SessionListResponse = serde_json::from_slice(&body(resp)?)?;
    Ok(envelope.sessions)
}

/// How the server should treat the new session.
///
/// The backend recognizes four modes. idtx-studio only ever *selects* the two
/// `*_edit` modes (it is an authoring tool); the `*_runtime` variants exist so
/// the enum matches the wire contract and are reserved for a future
/// idtx-runtime integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionMode {
    #[default]
    SingleEdit,
    SingleRuntime,
    CollaborativeEdit,
    CollaborativeRuntime,
}

impl SessionMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SessionMode::SingleEdit => "single_edit",
            SessionMode::SingleRuntime => "single_runtime",
            SessionMode::CollaborativeEdit => "collaborative_edit",
            SessionMode::CollaborativeRuntime => "collaborative_runtime",
        }
    }
}

/// Normalize a server file path to the forward-slash grammar the backend
/// enforces. The editor may build or receive a path with Windows `\`
/// separators when running on Windows; the session server rejects those with
/// "Path does not match allowed character set", so every path crossing to the
/// backend must use `/` regardless of host OS.
#[must_use]
pub fn normalize_server_path(path: &str) -> String {
    path.replace('\\', "/")
}

/// POST /api/v1/sessions and return the created session metadata.
pub fn create_session(
    base_url: &str,
    token: &str,
    usd_file: &str,
    mode: SessionMode,
) -> Result<SessionInfo, CollabError> {
    let url = format!("{}/api/v1/sessions", base_url.trim_end_matches('/'));
    // The server validates `usd_file` against a forward-slash path grammar and
    // rejects Windows `\` separators ("Path does not match allowed character
    // set: demo\simple_primitives.usda"). Normalize so a stage opened from a
    // subfolder works regardless of the host OS the editor runs on.
    let usd_file = normalize_server_path(usd_file);
    // `auto_commit` defaults to false server-side; sent explicitly for request
    // parity with the reference client. Not surfaced in the UI.
    let request_body =
        serde_json::json!({ "usd_file": usd_file, "mode": mode.as_str(), "auto_commit": false });
    let resp = client()?
        .post(&url)
        .timeout(std::time::Duration::from_secs(30))
        .bearer_auth(token)
        .json(&request_body)
        .send()?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(CollabError::Unauthorized);
    }
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body_text = String::from_utf8_lossy(&body(resp)?).into_owned();
        return Err(CollabError::Http {
            status,
            body: body_text,
        });
    }

    let info: SessionInfo = serde_json::from_slice(&body(resp)?)?;
    if !info.protocol.is_empty() && info.protocol != "protobuf-binary" {
        tracing::warn!(
            protocol = %info.protocol,
            "collab session reports an unexpected protocol; proceeding with protobuf-binary"
        );
    }
    Ok(info)
}

/// POST `/api/v1/sessions/{session_id}/commit` — merge session overrides back into the origin file.
pub fn commit_session(base_url: &str, token: &str, session_id: &str) -> Result<(), CollabError> {
    let url = format!(
        "{}/api/v1/sessions/{}/commit",
        base_url.trim_end_matches('/'),
        session_id
    );
    let resp = client()?
        .post(&url)
        .timeout(std::time::Duration::from_secs(30))
        .bearer_auth(token)
        .send()?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(CollabError::Unauthorized);
    }
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body_text = String::from_utf8_lossy(&body(resp)?).into_owned();
        return Err(CollabError::Http {
            status,
            body: body_text,
        });
    }
    Ok(())
}

/// Sends `DELETE /api/v1/sessions/{session_id}` to release the server slot.
pub fn delete_session(base_url: &str, token: &str, session_id: &str) -> Result<(), CollabError> {
    let url = format!(
        "{}/api/v1/sessions/{}",
        base_url.trim_end_matches('/'),
        session_id
    );
    let resp = client()?
        .delete(&url)
        .timeout(std::time::Duration::from_secs(30))
        .bearer_auth(token)
        .send()?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(CollabError::Unauthorized);
    }
    // 404 on DELETE is fine (already gone).
    Ok(())
}

/// Derive a WebSocket URL from a HTTP base URL and the relative `ws_url` path
/// returned by the session server.
///
/// `http://` → `ws://`, `https://` → `wss://`.
#[must_use]
pub fn ws_url_from_http(http_base: &str, ws_path: &str) -> String {
    let base = http_base.trim_end_matches('/');
    let (scheme, rest) = if let Some(r) = base.strip_prefix("https://") {
        ("wss", r)
    } else if let Some(r) = base.strip_prefix("http://") {
        ("ws", r)
    } else {
        // Pass through unknown schemes unchanged.
        return format!("{base}{ws_path}");
    };
    format!("{scheme}://{rest}{ws_path}")
}

#[cfg(test)]
mod tests {
    use super::{normalize_server_path, ws_url_from_http};

    #[test]
    fn normalize_server_path_rewrites_windows_separators() {
        assert_eq!(
            normalize_server_path("demo\\simple_primitives.usda"),
            "demo/simple_primitives.usda"
        );
        assert_eq!(normalize_server_path("a\\b\\c.usda"), "a/b/c.usda");
    }

    #[test]
    fn normalize_server_path_leaves_posix_paths_untouched() {
        assert_eq!(
            normalize_server_path("demo/simple_primitives.usda"),
            "demo/simple_primitives.usda"
        );
        assert_eq!(normalize_server_path("scene.usda"), "scene.usda");
    }

    #[test]
    fn http_becomes_ws() {
        assert_eq!(
            ws_url_from_http("http://server:8080", "/ws?sid=abc"),
            "ws://server:8080/ws?sid=abc"
        );
    }

    #[test]
    fn https_becomes_wss() {
        assert_eq!(
            ws_url_from_http("https://server.example.com", "/ws"),
            "wss://server.example.com/ws"
        );
    }

    #[test]
    fn trailing_slash_on_base_is_stripped() {
        assert_eq!(
            ws_url_from_http("http://localhost:7070/", "/ws/collab"),
            "ws://localhost:7070/ws/collab"
        );
    }
}
