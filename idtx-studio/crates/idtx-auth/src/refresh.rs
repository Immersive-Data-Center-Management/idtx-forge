//! Bounded recovery used on HTTP workers, never on the render loop.
use std::sync::{Arc, Mutex, OnceLock, atomic::{AtomicUsize, Ordering}};
use std::time::Duration;

pub type RefreshHandler = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
fn handler() -> &'static Mutex<Option<RefreshHandler>> {
    static HANDLER: OnceLock<Mutex<Option<RefreshHandler>>> = OnceLock::new();
    HANDLER.get_or_init(|| Mutex::new(None))
}
pub fn set_refresh_handler(value: Option<RefreshHandler>) {
    *handler().lock().unwrap_or_else(std::sync::PoisonError::into_inner) = value;
}
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
struct Permit;
impl Drop for Permit { fn drop(&mut self) { ACTIVE.fetch_sub(1, Ordering::AcqRel); } }

/// At most one retry is allowed by each transport. Provider work has four slots
/// and a five-second wait ceiling. Only this waiting caller can publish; a timed
/// out provider or a result superseded by logout/login never mutates credentials.
pub fn refresh_after_unauthorized(url: &str, rejected: Option<&str>, timeout: Duration) -> bool {
    let Some(rejected) = rejected else { return false; };
    let Ok(origin) = crate::Origin::from_url(url).map(|o| o.to_string()) else { return false; };
    let revision = crate::revision();
    let current = crate::authorization_header_for(&origin);
    if current.as_deref() != Some(rejected) { return current.is_some(); }
    let callback = handler().lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let Some(callback) = callback else {
        crate::revoke_rejected(&origin, rejected, revision);
        return false;
    };
    if ACTIVE.try_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < 4).then_some(n + 1)).is_err() { return false; }
    let permit = Permit;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let target = origin.clone();
    if std::thread::Builder::new().name("idtx-auth-refresh".into()).spawn(move || {
        let _permit = permit;
        let result = callback(&target);
        let _ = tx.send(result);
    }).is_err() { return false; }
    let result = rx.recv_timeout(timeout.min(Duration::from_secs(5))).ok().flatten();
    if let Some(token) = result {
        crate::publish_refresh(&origin, rejected, revision, token)
    } else {
        crate::revoke_rejected(&origin, rejected, revision);
        false
    }
}
