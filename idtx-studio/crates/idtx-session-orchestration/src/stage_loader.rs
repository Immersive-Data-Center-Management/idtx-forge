//! UI-neutral async stage loader.
//!
//! Lifts the editor shell's worker-thread stage-open machinery
//! (`idtx-studio-shell`'s `open_path` + `poll_loading_tabs`) into the runtime as a
//! reusable, egui/winit-free [`StageLoader`]: it opens a [`StudioSession`] on a
//! **separate** `StageService` on one bounded background worker. Private-session
//! writes can proceed concurrently through the two-tier C++ locking contract.
//! It delivers the prepared session over a one-slot channel the frame polls with
//! [`StageLoader::try_recv`].
//!
//! This is the async half of the per-level-assembly level transition
//! (`docs/level-transition-runtime-plan.md` §10): a transition opens the target
//! level's assembly here while the resident transfer scene keeps rendering, then
//! swaps the active session once the loader is ready.
//!
//! ```no_run
//! # use std::path::Path;
//! # use std::task::Poll;
//! # use idtx_session_orchestration::StageLoader;
//! # use idtx_studio_session::{RenderWorldUnitScale, TextureDecodeConfig};
//! # fn demo(path: &Path, device: wgpu::Device, queue: wgpu::Queue) {
//! let loader = StageLoader::request(
//!     path, device, queue, [1280, 720],
//!     TextureDecodeConfig::FULL_RESOLUTION, RenderWorldUnitScale::METRE, "",
//! );
//! // …each frame, non-blocking:
//! match loader.try_recv() {
//!     Poll::Pending => { /* still loading — keep rendering the transfer scene */ }
//!     Poll::Ready(Ok(session)) => { /* cut over to `session`, then drop the loader */ }
//!     Poll::Ready(Err(e)) => { /* surface `e`, stay on the current level */ }
//! }
//! # }
//! ```

use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, atomic::{AtomicBool, AtomicU8, Ordering}};
use std::time::{Duration, Instant};
use std::task::Poll;

use idtx_studio_session::{RenderWorldUnitScale, SessionError, StudioSession, TextureDecodeConfig};

/// Why an async stage open did not yield a session.
#[derive(Debug, thiserror::Error)]
pub enum StageLoadError {
    /// The session failed to open (USD compose / hydration error).
    #[error("stage open failed: {0}")]
    Open(#[from] SessionError),
    /// The loader thread terminated without sending a result (it panicked).
    #[error("stage loader worker thread terminated without a result")]
    WorkerLost,
    #[error("another stage load is still running")]
    Busy,
    #[error("stage load cancelled")]
    Cancelled,
    #[error("stage load deadline exceeded")]
    TimedOut,
}

pub type PrepareSession = Arc<dyn Fn(&mut StudioSession) + Send + Sync>;

#[derive(Clone)]
pub struct LoadPreparation {
    pub configure: Option<PrepareSession>,
    pub prepare: Option<PrepareSession>,
    pub state: Option<idtx_behavior::ExperienceState>,
    pub timeout: Duration,
}

impl Default for LoadPreparation {
    fn default() -> Self { Self { configure: None, prepare: None, state: None, timeout: Duration::from_secs(30) } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadPhase { Opening, Preparing, Ready, Cancelled, Failed }

static LOADING: AtomicBool = AtomicBool::new(false);
struct LoadPermit;
impl Drop for LoadPermit { fn drop(&mut self) { LOADING.store(false, Ordering::Release); } }
struct PreparedSession { session: StudioSession, _permit: LoadPermit }

/// A background stage open in flight. Poll [`try_recv`](Self::try_recv) each frame;
/// once it returns [`Poll::Ready`], stop polling and drop the loader — the channel
/// is one-shot (a further poll reports [`StageLoadError::WorkerLost`]).
pub struct StageLoader {
    rx: Option<mpsc::Receiver<Result<PreparedSession, StageLoadError>>>,
    label: String,
    cancelled: Arc<AtomicBool>,
    phase: Arc<AtomicU8>,
    deadline: Instant,
    admitted: bool,
}

impl StageLoader {
    pub fn active_loads() -> usize { usize::from(LOADING.load(Ordering::Acquire)) }
    /// Spawn a background open of the stage at `path` and return the in-flight
    /// loader. Mirrors [`StudioSession::open_with_gpu_and_session_layer`]: the
    /// `wgpu` device/queue are cloned (cheap `Arc` handles) and shared with the
    /// live session; `session_layer_text` is the strongest anonymous session layer
    /// composed at open (`""` for none — asset paths inside it must be absolute).
    ///
    /// The worker opens a separate stage and warms resources before handoff.
    /// Admission, readiness and worker failures are returned through the poll.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn request(
        path: &Path,
        device: wgpu::Device,
        queue: wgpu::Queue,
        viewport: [u32; 2],
        texture_config: TextureDecodeConfig,
        world_unit_scale: RenderWorldUnitScale,
        session_layer_text: &str,
    ) -> Self {
        Self::request_prepared(path, device, queue, viewport, texture_config, world_unit_scale, session_layer_text, LoadPreparation::default())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn request_prepared(path: &Path, device: wgpu::Device, queue: wgpu::Queue, viewport: [u32; 2],
        texture_config: TextureDecodeConfig, world_unit_scale: RenderWorldUnitScale, session_layer_text: &str,
        preparation: LoadPreparation) -> Self {
        let label = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("stage")
            .to_owned();
        let path = path.to_path_buf();
        let session_layer_text = session_layer_text.to_owned();
        let (tx, rx) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let phase = Arc::new(AtomicU8::new(0));
        let deadline = Instant::now() + preparation.timeout;
        if LOADING.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            let _ = tx.send(Err(StageLoadError::Busy));
            phase.store(4, Ordering::Release);
            return Self { rx: Some(rx), label, cancelled, phase, deadline, admitted: false };
        }
        let permit = LoadPermit;
        let stop = cancelled.clone();
        let progress = phase.clone();
        let _ = std::thread::Builder::new()
            .name("idtx-stage-loader".to_owned())
            .spawn(move || {
                let check = || {
                    if stop.load(Ordering::Acquire) { Err(StageLoadError::Cancelled) }
                    else if Instant::now() >= deadline { Err(StageLoadError::TimedOut) }
                    else { Ok(()) }
                };
                let result = (|| {
                check()?;
                let mut session = StudioSession::open_with_gpu_and_session_layer(
                    &path,
                    device,
                    queue,
                    viewport,
                    texture_config,
                    world_unit_scale,
                    &session_layer_text,
                )?;
                check()?;
                progress.store(1, Ordering::Release);
                if let Some(state) = preparation.state { session.set_experience_state(state); }
                if let Some(configure) = preparation.configure {
                    configure(&mut session);
                    session.validate_runtime_readiness()?;
                } else { session.stage().validate_runtime_readiness().map_err(SessionError::from)?; }
                check()?;
                session.render_one_frame(false);
                if let Some(prepare) = preparation.prepare { prepare(&mut session); }
                check()?;
                progress.store(2, Ordering::Release);
                Ok(PreparedSession { session, _permit: permit })
                })();
                if result.is_err() { progress.store(if stop.load(Ordering::Acquire) { 3 } else { 4 }, Ordering::Release); }
                // If the receiver was dropped (loader cancelled), discard quietly.
                tx.send(result).ok();
            });
        Self { rx: Some(rx), label, cancelled, phase, deadline, admitted: true }
    }

    /// Non-blocking poll. [`Poll::Pending`] while the open is still in flight;
    /// [`Poll::Ready`]`(Ok(session))` once ready; [`Poll::Ready`]`(Err(..))` on an
    /// open failure or a crashed worker. One-shot: stop polling after `Ready`.
    pub fn try_recv(&self) -> Poll<Result<StudioSession, StageLoadError>> {
        if self.cancelled.load(Ordering::Acquire) { return Poll::Ready(Err(StageLoadError::Cancelled)); }
        if Instant::now() >= self.deadline { self.cancel(); return Poll::Ready(Err(StageLoadError::TimedOut)); }
        let Some(rx) = &self.rx else { return Poll::Ready(Err(StageLoadError::WorkerLost)); };
        match rx.try_recv() {
            Ok(result) => Poll::Ready(result.map(|prepared| prepared.session)),
            Err(mpsc::TryRecvError::Empty) => Poll::Pending,
            Err(mpsc::TryRecvError::Disconnected) => Poll::Ready(Err(StageLoadError::WorkerLost)),
        }
    }

    pub fn cancel(&self) {
        if self.admitted && self.phase.load(Ordering::Acquire) < 2 { idtx_auth::cancel_pending_refresh(); }
        self.cancelled.store(true, Ordering::Release); self.phase.store(3, Ordering::Release);
    }

    #[must_use]
    pub fn phase(&self) -> LoadPhase {
        match self.phase.load(Ordering::Acquire) { 0 => LoadPhase::Opening, 1 => LoadPhase::Preparing, 2 => LoadPhase::Ready, 3 => LoadPhase::Cancelled, _ => LoadPhase::Failed }
    }

    /// A short human-readable label for the stage being loaded (its file name).
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

impl Drop for StageLoader {
    fn drop(&mut self) {
        self.cancel();
        // An in-flight open is not interruptible; it observes cancellation before
        // configuration/publication. Ready-session teardown also stays off-frame.
        if self.admitted && let Some(rx) = self.rx.take() {
            std::thread::spawn(move || drop(rx));
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/support/mod.rs"]
mod test_support;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use super::test_support as support;

    #[test]
    #[ignore = "requires OpenUSD link + headless GPU"]
    fn unresolved_payloads_fail_readiness_even_when_root_open_succeeds() {
        let scratch = support::scratch_dir("readiness");
        let path = scratch.path().join("level.usda");
        std::fs::write(&path, "#usda 1.0\ndef Xform \"World\" (prepend payload = @missing.usda@) {}\n").unwrap();
        let stage = idtx_usd::StageService::open_with_session(&path, "", true).expect("USD root open alone can succeed");
        assert!(stage.validate_runtime_readiness().is_err());
        drop(stage);
        let (device, queue) = crate::headless_device().unwrap();
        let loader = StageLoader::request(&path, device, queue, [8, 8], TextureDecodeConfig::default(), RenderWorldUnitScale::METRE, "");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match loader.try_recv() {
                Poll::Pending => { assert!(Instant::now() < deadline); std::thread::sleep(Duration::from_millis(2)); }
                Poll::Ready(Err(StageLoadError::Open(_))) => break,
                other @ Poll::Ready(_) => panic!("unready stage was published: {other:?}"),
            }
        }
    }

    /// A missing stage surfaces as `Poll::Ready(Err(Open(..)))` — not a hang or a
    /// panic. Links `OpenUSD` + a headless GPU, so it is ignored by default; run with
    /// the USD env configured (`IDTX_USD_DIR`, `DYLD_LIBRARY_PATH`).
    #[test]
    #[ignore = "requires OpenUSD link + headless GPU"]
    fn missing_stage_reports_open_error() {
        let (device, queue) = crate::headless_device().expect("headless device");
        let loader = StageLoader::request(
            &PathBuf::from("/nonexistent/does-not-exist.usda"),
            device,
            queue,
            [64, 64],
            TextureDecodeConfig::FULL_RESOLUTION,
            RenderWorldUnitScale::METRE,
            "",
        );
        assert_eq!(loader.label(), "does-not-exist.usda");
        // Block until the worker reports (bounded so a hang fails rather than spins).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match loader.try_recv() {
                Poll::Pending => {
                    assert!(std::time::Instant::now() < deadline, "loader hung");
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Poll::Ready(Err(StageLoadError::Open(_))) => break,
                other @ Poll::Ready(_) => panic!("expected an Open error, got {other:?}"),
            }
        }
    }
}
