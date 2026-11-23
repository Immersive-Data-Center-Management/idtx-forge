//! One ordered asset preparation per session, four across the process. Retain
//! the prepared stage until composition so `OpenUSD`'s weak layer registry stays warm.
use std::{path::Path, sync::{atomic::{AtomicUsize, Ordering}, mpsc}, time::{Duration, Instant}};
use idtx_usd::StageService;
use crate::SessionError;

static WORKERS: AtomicUsize = AtomicUsize::new(0);
struct Permit;
impl Drop for Permit { fn drop(&mut self) { WORKERS.fetch_sub(1, Ordering::AcqRel); } }

#[derive(Debug)]
pub(crate) struct Preparation {
    pub path: String,
    receiver: mpsc::Receiver<Result<StageService, String>>,
    started: Instant,
}

impl Preparation {
    pub fn start(root: &str, path: &str) -> Result<Self, SessionError> {
        if path.is_empty() { return Err(SessionError::Collaboration("empty reference path".into())); }
        WORKERS.try_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < 4).then_some(n + 1))
            .map_err(|_| SessionError::Collaboration("asset preparation capacity reached; retry application".into()))?;
        let permit = Permit;
        let resolved = if path.contains("://") || Path::new(path).is_absolute() { path.to_owned() }
            else if let Ok(root) = url::Url::parse(root) {
                root.join(path).map_err(|error| SessionError::Collaboration(error.to_string()))?.to_string()
            } else { Path::new(root).parent().unwrap_or(Path::new(".")).join(path).to_string_lossy().into_owned() };
        let (reply, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new().name("idtx-collab-asset".into()).spawn(move || {
            let _permit = permit;
            let _ = reply.send(StageService::open_with_session(resolved, "", true).map_err(|error| error.to_string()));
        }).map_err(|error| SessionError::Collaboration(error.to_string()))?;
        Ok(Self { path: path.into(), receiver, started: Instant::now() })
    }

    pub fn poll(&self) -> Option<Result<StageService, SessionError>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result.map_err(SessionError::Collaboration)),
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(SessionError::Collaboration("asset preparation worker stopped".into()))),
            Err(mpsc::TryRecvError::Empty) if self.started.elapsed() >= Duration::from_secs(120) =>
                Some(Err(SessionError::Collaboration("asset preparation deadline exceeded".into()))),
            Err(mpsc::TryRecvError::Empty) => None,
        }
    }
}
