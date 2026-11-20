#[derive(Debug, thiserror::Error)]
pub enum CollabError {
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    #[error("unauthorized")]
    Unauthorized,
    #[error("websocket: {0}")]
    WebSocket(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("request: {0}")]
    Request(#[from] reqwest::Error),
    #[error("encode: {0}")]
    Encode(#[from] prost::EncodeError),
    #[error("invalid response: {0}")]
    Json(#[from] serde_json::Error),
}
