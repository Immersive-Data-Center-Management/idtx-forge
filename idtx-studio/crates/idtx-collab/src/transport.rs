//! Bounded connection workers. Socket shutdown interrupts TLS/read/write waits;
//! DNS uses the shared fixed worker pool and a five-second total open deadline.
use std::{net::{Shutdown, TcpStream}, sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}, mpsc}, time::{Duration, Instant}};
use tungstenite::{WebSocket, stream::MaybeTlsStream};
use crate::error::CollabError;

// Core's attribute batches and snapshot defaults may contain mesh arrays.
// Keep the same bounded message budget as its attribute dispatcher.
pub(crate) const MAX_FRAME: usize = 8 * 1024 * 1024;
pub(crate) type Control = Arc<Mutex<Option<TcpStream>>>;
static WORKERS: AtomicUsize = AtomicUsize::new(0);
pub(crate) struct Permit;
impl Permit {
    pub fn acquire() -> Result<Self, CollabError> {
        WORKERS.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < 16).then_some(n + 1))
            .map(|_| Self).map_err(|_| CollabError::WebSocket("connection worker limit reached".into()))
    }
}
impl Drop for Permit { fn drop(&mut self) { WORKERS.fetch_sub(1, Ordering::AcqRel); } }

pub(crate) fn stop(control: &Control) {
    if let Ok(socket) = control.lock() && let Some(socket) = &*socket { let _ = socket.shutdown(Shutdown::Both); }
}

pub(crate) fn connect(url: &str, token: &str, control: &Control) -> Result<WebSocket<MaybeTlsStream<TcpStream>>, CollabError> {
    let fail = || CollabError::WebSocket("WebSocket connection failed or timed out".into());
    let uri: tungstenite::http::Uri = url.parse().map_err(|_| fail())?;
    let scheme = uri.scheme_str().ok_or_else(fail)?;
    if !matches!(scheme, "ws" | "wss") { return Err(fail()); }
    let host = uri.host().ok_or_else(fail)?.trim_matches(['[', ']']);
    let port = uri.port_u16().unwrap_or(if scheme == "wss" { 443 } else { 80 });
    let deadline = Instant::now() + Duration::from_secs(5);
    let addresses = idtx_net_guard::dns::resolve(host, port, Duration::from_secs(5)).map_err(|_| fail())?;
    let mut connected = None;
    for address in addresses {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() { break; }
        if let Ok(stream) = TcpStream::connect_timeout(&address, remaining) { connected = Some(stream); break; }
    }
    let stream = connected.ok_or_else(fail)?;
    stream.set_read_timeout(Some(Duration::from_secs(1))).map_err(|_| fail())?;
    stream.set_write_timeout(Some(Duration::from_millis(100))).map_err(|_| fail())?;
    *control.lock().map_err(|_| fail())? = Some(stream.try_clone().map_err(|_| fail())?);
    let watchdog = stream.try_clone().map_err(|_| fail())?;
    let (done, wait) = mpsc::sync_channel::<()>(1);
    std::thread::spawn(move || {
        if wait.recv_timeout(deadline.saturating_duration_since(Instant::now())) == Err(mpsc::RecvTimeoutError::Timeout) {
            let _ = watchdog.shutdown(Shutdown::Both);
        }
    });
    let request = tungstenite::ClientRequestBuilder::new(uri).with_header("Authorization", format!("Bearer {token}"));
    let config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME)).max_frame_size(Some(MAX_FRAME))
        .write_buffer_size(0).max_write_buffer_size(MAX_FRAME * 2);
    let result = tungstenite::client_tls_with_config(request, stream, Some(config), None).map_err(|_| fail());
    let _ = done.try_send(());
    let (mut socket, _) = result?;
    super::client::set_read_timeout(&mut socket, Some(Duration::from_millis(100)))?;
    // Steady-state writes must never wait on a non-reading peer. A partial
    // write/WouldBlock is indeterminate and stops synchronization, not replayed.
    if let Some(stream) = &*control.lock().map_err(|_| fail())? {
        stream.set_nonblocking(true).map_err(|_| fail())?;
    }
    Ok(socket)
}
