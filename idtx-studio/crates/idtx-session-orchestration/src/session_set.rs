//! UI-neutral multi-session container.
//!
//! Today the runtime holds exactly one [`StudioSession`]. A level transition
//! generalises that to a small *set*: the `current` level being played, plus an
//! optional resident `transfer` (mask) session that renders while the next level
//! loads async. [`SessionSet`] owns both and tracks which one the per-frame loop
//! ticks + renders via [`ActiveSession`].
//!
//! This is the storage + routing primitive only — it does **not** itself drive a
//! transition (that is the transition driver, which drops `current`, spawns a
//! [`crate::StageLoader`], and calls [`SessionSet::set_current`] on completion).
//! For a single-stage experience (`transfer = None`) the set stays permanently on
//! [`ActiveSession::Level`] and behaves exactly like the old single `session`
//! field, so single-stage runs are byte-identical.
//!
//! See `docs/level-transition-runtime-plan.md` §2.

use idtx_studio_session::StudioSession;

/// Which session the per-frame loop currently ticks + renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveSession {
    /// The live level being played ([`SessionSet::current`]).
    Level,
    /// The resident transfer/mask scene ([`SessionSet::transfer`]), shown while a
    /// new level loads. Only reachable when a transfer session exists.
    Transfer,
}

/// The runtime's live session set: the current level plus an optional resident
/// transfer scene, and which of the two is [`active`](Self::active).
///
/// A single-stage experience constructs this with `transfer = None`; the active
/// session then never leaves [`ActiveSession::Level`] and [`show_transfer`] is a
/// no-op, so the frame loop ticks/renders the one level exactly as before.
///
/// [`show_transfer`]: Self::show_transfer
pub struct SessionSet {
    current: StudioSession,
    transfer: Option<StudioSession>,
    active: ActiveSession,
    /// Bumped on every change to **which session (and backing `display_view`)
    /// is active**: a cut to the transfer scene, a cut back, or a `set_current`
    /// swap. The windowed frame loop watches this to invalidate its cached
    /// present bind group — the [`crate::present::Presenter`] caches the blit
    /// bind group keyed on the source view and rebuilds it only when told the
    /// source changed. A swap hands a *different* `display_view` of the *same
    /// size* (no resize), so without this epoch the presenter would keep
    /// blitting the **old** session's last frame (a frozen screen) while the new
    /// session renders unseen. See `active_epoch`.
    epoch: u64,
}

impl SessionSet {
    /// Build a set over the already-opened `current` level and an optional
    /// resident `transfer` session. The set starts on [`ActiveSession::Level`].
    #[must_use]
    pub fn new(current: StudioSession, transfer: Option<StudioSession>) -> Self {
        Self {
            current,
            transfer,
            active: ActiveSession::Level,
            epoch: 0,
        }
    }

    /// A monotonically increasing counter that changes whenever the active
    /// session (and thus its backing `display_view`) changes — a transfer cut,
    /// a cut back, or a `set_current` swap. The windowed loop invalidates its
    /// cached present bind group when this differs from the last presented
    /// value; see the field doc and [`crate::present::Presenter`].
    #[must_use]
    pub fn active_epoch(&self) -> u64 {
        self.epoch
    }

    /// Which session the loop should tick + render this frame.
    #[must_use]
    pub fn active(&self) -> ActiveSession {
        self.active
    }

    /// `true` when a resident transfer session exists (a multi-level experience
    /// configured with a mask scene).
    #[must_use]
    pub fn has_transfer(&self) -> bool {
        self.transfer.is_some()
    }

    /// The session the per-frame loop should drive: the transfer session when the
    /// set is [`ActiveSession::Transfer`] **and** a transfer session exists, else
    /// the current level. (A `Transfer` active with no transfer session — which
    /// [`show_transfer`](Self::show_transfer) never produces — falls back to
    /// `current` so the loop can never dereference a missing session.)
    #[must_use]
    pub fn active_session(&self) -> &StudioSession {
        match (self.active, self.transfer.as_ref()) {
            (ActiveSession::Transfer, Some(transfer)) => transfer,
            _ => &self.current,
        }
    }

    /// Mutable twin of [`active_session`](Self::active_session).
    pub fn active_session_mut(&mut self) -> &mut StudioSession {
        match (self.active, self.transfer.as_mut()) {
            (ActiveSession::Transfer, Some(transfer)) => transfer,
            _ => &mut self.current,
        }
    }

    /// The current level session.
    #[must_use]
    pub fn current(&self) -> &StudioSession {
        &self.current
    }

    /// Mutable access to the current level session.
    pub fn current_mut(&mut self) -> &mut StudioSession {
        &mut self.current
    }

    /// The resident transfer session, if one exists.
    #[must_use]
    pub fn transfer(&self) -> Option<&StudioSession> {
        self.transfer.as_ref()
    }

    /// Mutable access to the resident transfer session, if one exists.
    pub fn transfer_mut(&mut self) -> Option<&mut StudioSession> {
        self.transfer.as_mut()
    }

    /// Cut to the transfer/mask scene — **only** if one exists (otherwise a
    /// no-op, so a single-stage set can never end up active on a missing
    /// session). Used by the transition driver at the start of a transition.
    pub fn show_transfer(&mut self) {
        if self.transfer.is_some() && self.active != ActiveSession::Transfer {
            self.active = ActiveSession::Transfer;
            self.epoch = self.epoch.wrapping_add(1);
        }
    }

    /// Cut back to the current level ([`ActiveSession::Level`]).
    pub fn show_current(&mut self) {
        if self.active != ActiveSession::Level {
            self.active = ActiveSession::Level;
            self.epoch = self.epoch.wrapping_add(1);
        }
    }

    /// Replace the current level session and return to [`ActiveSession::Level`],
    /// **returning the displaced outgoing session** so the caller can tear its
    /// behaviors down against its own ECS world before dropping it. This is the
    /// Cutover primitive: once the [`crate::StageLoader`] hands back the newly
    /// loaded level, swap it in here and the transfer scene stops rendering.
    ///
    /// The returned session is the *old* `current`. A caller that drives its own
    /// behavior host (not the session-owned one — the visionOS/XR runtime) must
    /// detach that host against `outgoing.world_mut()` **before** dropping it;
    /// dropping it here instead would run the guest teardown hooks against the
    /// new level's world, where stale entity handles can resolve to unrelated
    /// entities (two fresh `bevy_ecs` worlds mint colliding `Entity` ids). A
    /// caller that uses the session-owned host can simply drop the return value —
    /// `StudioSession`'s `Drop` tears the host down against its own world.
    pub fn set_current(&mut self, current: StudioSession) -> StudioSession {
        let outgoing = std::mem::replace(&mut self.current, current);
        self.active = ActiveSession::Level;
        // Always bump: `current` is a brand-new session with a new `display_view`,
        // even when `active` was already `Level` (an abort-and-reload could leave
        // it so). The presenter must rebuild its bind group against the new view.
        self.epoch = self.epoch.wrapping_add(1);
        outgoing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};
    use std::path::Path;

    /// Write a one-prim `.usda` stage to `path` whose `/World/<prim>` is unique to
    /// that stage, so a session can be fingerprinted by which prim composes.
    fn write_stage(path: &Path, prim: &str) {
        let text = format!(
            "#usda 1.0\n(\n    defaultPrim = \"World\"\n    upAxis = \"Y\"\n    metersPerUnit = 1\n)\ndef Xform \"World\"\n{{\n    def Xform \"{prim}\" {{}}\n}}\n"
        );
        std::fs::write(path, text).expect("write test stage");
    }

    /// Open a stage headlessly and warm one frame (mirrors the runtime open path).
    fn open(path: &Path, device: &wgpu::Device, queue: &wgpu::Queue) -> StudioSession {
        let mut session = StudioSession::open_with_gpu(
            path,
            device.clone(),
            queue.clone(),
            [64, 64],
            TextureDecodeConfig::FULL_RESOLUTION,
            RenderWorldUnitScale::METRE,
        )
        .expect("open stage");
        session.render_one_frame(false);
        session
    }

    /// A multi-level [`SessionSet`] routes `active_session` between the start level
    /// and the resident transfer scene, and a single-stage config opens no transfer.
    ///
    /// Links `OpenUSD` + a headless GPU, so it is ignored by default; run with the USD
    /// env configured (`IDTX_USD_DIR`, `DYLD_LIBRARY_PATH`).
    #[test]
    #[ignore = "requires OpenUSD link + headless GPU"]
    fn session_set_routes_active_between_level_and_transfer() {
        let (device, queue) = crate::headless_device().expect("headless device");
        let dir = std::env::temp_dir().join(format!("idtx-session-set-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let start = dir.join("start.usda");
        let transfer = dir.join("transfer.usda");
        write_stage(&start, "Start");
        write_stage(&transfer, "Loader");

        // --- Multi-level: a transfer/mask scene is declared. ---
        let current = open(&start, &device, &queue);
        // Open the transfer session exactly as run_headless does (only when declared).
        let transfer_session = Some(open(&transfer, &device, &queue));
        let mut sessions = SessionSet::new(current, transfer_session);

        assert!(sessions.has_transfer(), "transfer session must exist");
        assert_eq!(sessions.active(), ActiveSession::Level);
        let epoch0 = sessions.active_epoch();
        sessions.active_session_mut().render_one_frame(false);
        assert!(
            sessions
                .active_session()
                .stage()
                .prim_at_path("/World/Start")
                .is_some(),
            "active (Level) session must expose /World/Start"
        );

        // Cut to the transfer scene: the active session now exposes the loader prim.
        sessions.show_transfer();
        assert_eq!(sessions.active(), ActiveSession::Transfer);
        let epoch1 = sessions.active_epoch();
        assert!(
            epoch1 != epoch0,
            "a transfer cut must bump the active epoch (presenter invalidation)"
        );
        // A redundant cut (already on the transfer scene) must NOT bump the epoch.
        sessions.show_transfer();
        assert_eq!(
            sessions.active_epoch(),
            epoch1,
            "a no-op transfer cut must not bump the epoch"
        );
        sessions.active_session_mut().render_one_frame(false);
        assert!(
            sessions
                .active_session()
                .stage()
                .prim_at_path("/World/Loader")
                .is_some(),
            "active (Transfer) session must expose /World/Loader"
        );
        assert!(
            sessions
                .active_session()
                .stage()
                .prim_at_path("/World/Start")
                .is_none(),
            "the transfer session must not carry the start level's prim"
        );

        // Cut back to the live level.
        sessions.show_current();
        assert_eq!(sessions.active(), ActiveSession::Level);
        assert!(
            sessions.active_epoch() != epoch1,
            "cutting back to the level must bump the active epoch"
        );
        assert!(
            sessions
                .active_session()
                .stage()
                .prim_at_path("/World/Start")
                .is_some(),
            "back on the level, active must expose /World/Start again"
        );

        // --- Single-stage: no transfer declared → no transfer session. ---
        let only = open(&start, &device, &queue);
        let no_transfer: Option<StudioSession> = None;
        let mut single = SessionSet::new(only, no_transfer);
        assert!(
            !single.has_transfer(),
            "single-stage config must open no transfer session"
        );
        // show_transfer is a no-op with no transfer scene — stays on the level.
        let single_epoch = single.active_epoch();
        single.show_transfer();
        assert_eq!(single.active(), ActiveSession::Level);
        assert_eq!(
            single.active_epoch(),
            single_epoch,
            "a transfer-less set must not bump the epoch on a no-op cut"
        );
        // A `set_current` swap always bumps — the new session has a new view.
        let swapped_in = open(&start, &device, &queue);
        single.set_current(swapped_in);
        assert!(
            single.active_epoch() != single_epoch,
            "set_current must bump the epoch (new session, new display_view)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
