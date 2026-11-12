//! Off-thread convex-hull generation for the collision inspector's
//! **Generate CollisionHull** button.
//!
//! The quickhull compute ([`idtx_hull_gen::generate_convex_hull`]) can run for a
//! while on dense meshes, so it runs on a worker thread while the egui UI stays
//! responsive and shows an animated spinner (the shell repaints every frame
//! while a job is live). The shell polls the [`HullJob`] each frame; on
//! completion it authors `idtx:collision:hull:points` / `:triangles` onto the
//! prim (see `actions.rs`).

use std::sync::mpsc::{Receiver, TryRecvError};

use idtx_hull_gen::{HullParams, HullResult};

/// Outcome of a finished hull-generation worker.
pub(crate) enum HullOutcome {
    /// The hull was computed — boundary vertices + triangle-index soup.
    Done(HullResult),
    /// A human-facing failure message (too few points, degenerate cloud, …).
    Failed(String),
}

/// A running background hull generation, owned by the shell and polled each
/// frame. Carries the target tab + prim path so the poll loop authors the
/// result onto the right prim, and the inspector can show the spinner for it.
pub(crate) struct HullJob {
    rx: Receiver<HullOutcome>,
    pub(crate) tab_id: u64,
    pub(crate) path: String,
}

impl HullJob {
    /// Non-blocking poll. Returns `Some` exactly once, when the worker finishes
    /// (or the thread died). `None` means still running.
    pub(crate) fn poll(&self) -> Option<HullOutcome> {
        match self.rx.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(HullOutcome::Failed(
                "hull generation thread ended unexpectedly".to_owned(),
            )),
        }
    }
}

/// Spawn a worker computing the convex hull of `points` (collision-prim local
/// space) under `params`, tagged with the owning `tab_id` / prim `path`.
pub(crate) fn spawn(
    points: Vec<[f32; 3]>,
    params: HullParams,
    tab_id: u64,
    path: String,
) -> HullJob {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = match idtx_hull_gen::generate_convex_hull(&points, &params) {
            Ok(result) => HullOutcome::Done(result),
            Err(e) => HullOutcome::Failed(e.to_string()),
        };
        // Receiver may have been dropped (tab closed); ignore the send error.
        let _ = tx.send(outcome);
    });
    HullJob { rx, tab_id, path }
}
