//! Per-stage animation clock and per-prim animated-attribute hydration.
//!
//! # Playback model
//!
//! One [`StagePlayback`] component on the pseudo-root entity drives the
//! global animation clock.  The app frame loop calls
//! [`StagePlayback::advance`] each tick, then the session calls
//! `service.sample_viewport_roots(current_tc, &animated_prim_ids)` and
//! pipes the result through the normal `SceneHydrator::apply_viewport_projection`
//! path.  All animated domains (transform, visibility, display-color,
//! skeletal poses) are therefore updated through the existing C++
//! `project_viewport` machinery — no raw per-attribute FFI reads needed.
//!
//! # Inspector support
//!
//! [`AnimatedAttribute`](crate::AnimatedAttribute) is attached to every
//! entity with at least one time-sampled attribute.  The animation
//! inspector walks this component to show the track list for a selected
//! prim and for the whole stage via [`StageAnimClip`].

use bevy_ecs::prelude::*;
use idtx_usd::{PrimId, StageAnimation, StageService, TimeSamples};

use crate::animation_catalog::AnimationCatalog;
use crate::components::{AnimTrack, AnimatedAttribute, PrimNode, TrackTarget};

// ---------------------------------------------------------------------------
// StageAnimClip — stage-level summary built once at open / re-hydration
// ---------------------------------------------------------------------------

/// High-level summary of an entire stage's animation.
///
/// Attached to the pseudo-root entity at hydration. Rebuilt whenever the
/// stage `info_changed` flag is set in a change batch.
#[derive(Component, Debug, Clone, Default)]
pub struct StageAnimClip {
    /// True when the stage has authored time codes or any animated prims.
    pub has_animation: bool,
    pub start_time_code: f64,
    pub end_time_code: f64,
    pub time_codes_per_second: f64,
    pub track_count: usize,
    /// Flat list of `(prim_path, track)` sorted by path then name, for the
    /// animation inspector panel.
    pub tracks: Vec<(String, AnimTrack)>,
}

// ---------------------------------------------------------------------------
// Shared time-code integrator
// ---------------------------------------------------------------------------

/// Core real-time → time-code integrator shared by [`StagePlayback`] (the
/// single stage-global clock) and [`ClipPlayhead`] (one of possibly many
/// per-entity playheads).
///
/// Advances `current` by `dt_seconds` of real time and returns
/// `(next_time_code, still_playing)`:
/// - a **looping** clip wraps within `[start, end)` via `rem_euclid` and stays
///   playing;
/// - a **non-looping** clip clamps to `[start, end]` and reports
///   `still_playing == false` once it reaches the bound it is travelling toward
///   (the `end` for forward `speed`, the `start` for reverse `speed`).
///
/// Callers must have already verified the range is non-empty (`end > start`);
/// with an empty range the result is `(current, true)`.
fn integrate_time_code(
    current: f64,
    start: f64,
    end: f64,
    time_codes_per_second: f64,
    speed: f64,
    looping: bool,
    dt_seconds: f64,
) -> (f64, bool) {
    let delta_tc = dt_seconds * time_codes_per_second * speed;
    let mut next = current + delta_tc;
    let span = (end - start).max(0.0);
    if looping && span > 0.0 {
        let rel = next - start;
        next = start + rel.rem_euclid(span);
        (next, true)
    } else {
        next = next.clamp(start, end);
        let mut playing = true;
        if speed >= 0.0 && next >= end {
            next = end;
            playing = false;
        } else if speed < 0.0 && next <= start {
            next = start;
            playing = false;
        }
        (next, playing)
    }
}

// ---------------------------------------------------------------------------
// StagePlayback — per-stage playback clock
// ---------------------------------------------------------------------------

/// Per-stage playback clock attached to the pseudo-root entity.
///
/// Starts paused at `current = start`. The app frame loop calls
/// [`advance`](StagePlayback::advance) each tick. USD is never written; all
/// animation data is transient render state only.
#[derive(Component, Debug, Clone, Copy)]
pub struct StagePlayback {
    pub playing: bool,
    pub current_time_code: f64,
    pub start_time_code: f64,
    pub end_time_code: f64,
    pub time_codes_per_second: f64,
    pub speed: f64,
    pub looping: bool,
    pub last_sampled_time_code: f64,
    pub needs_initial_sample: bool,
}

impl Default for StagePlayback {
    fn default() -> Self {
        Self {
            playing: false,
            current_time_code: 0.0,
            start_time_code: 0.0,
            end_time_code: 0.0,
            time_codes_per_second: 24.0,
            speed: 1.0,
            looping: true,
            last_sampled_time_code: f64::NAN,
            needs_initial_sample: true,
        }
    }
}

impl StagePlayback {
    #[must_use]
    pub fn from_clip(clip: &StageAnimClip) -> Self {
        let tps = if clip.time_codes_per_second > 0.0 {
            clip.time_codes_per_second
        } else {
            24.0
        };
        Self {
            playing: false,
            current_time_code: clip.start_time_code,
            start_time_code: clip.start_time_code,
            end_time_code: clip.end_time_code,
            time_codes_per_second: tps,
            speed: 1.0,
            looping: true,
            last_sampled_time_code: f64::NAN,
            needs_initial_sample: true,
        }
    }

    #[must_use]
    pub fn duration_time_codes(&self) -> f64 {
        (self.end_time_code - self.start_time_code).max(0.0)
    }

    #[must_use]
    pub fn has_range(&self) -> bool {
        self.end_time_code > self.start_time_code
    }

    /// Advance the clock by `dt_seconds` of real time.  No-op when paused.
    pub fn advance(&mut self, dt_seconds: f64) {
        if !self.playing || !self.has_range() {
            return;
        }
        let (next, playing) = integrate_time_code(
            self.current_time_code,
            self.start_time_code,
            self.end_time_code,
            self.time_codes_per_second,
            self.speed,
            self.looping,
            dt_seconds,
        );
        self.current_time_code = next;
        self.playing = playing;
    }

    pub fn scrub_to(&mut self, tc: f64) {
        self.current_time_code = if self.has_range() {
            tc.clamp(self.start_time_code, self.end_time_code)
        } else {
            self.start_time_code
        };
    }

    pub fn stop(&mut self) {
        self.playing = false;
        self.current_time_code = self.start_time_code;
    }

    /// True when a new USD sample is needed this frame.
    #[must_use]
    pub fn sample_pending(&self) -> bool {
        if self.needs_initial_sample || self.last_sampled_time_code.is_nan() {
            return true;
        }
        (self.last_sampled_time_code - self.current_time_code).abs() > 1e-9
    }

    pub fn mark_sampled(&mut self) {
        self.last_sampled_time_code = self.current_time_code;
        self.needs_initial_sample = false;
    }
}

// ---------------------------------------------------------------------------
// PlaybackRequest — UI intent
// ---------------------------------------------------------------------------

/// UI-emitted playback command.
#[derive(Debug, Clone, Copy)]
pub enum PlaybackRequest {
    TogglePlay,
    Play,
    Pause,
    Stop,
    Scrub(f64),
    SetLooping(bool),
}

/// Apply `req` to the [`StagePlayback`] on `pseudo_root`.
/// Returns `true` when an existing clock was found and updated.
pub fn apply_playback_request(
    world: &mut World,
    pseudo_root: Entity,
    req: PlaybackRequest,
) -> bool {
    let Some(mut clock) = world.get_mut::<StagePlayback>(pseudo_root) else {
        return false;
    };
    match req {
        PlaybackRequest::TogglePlay => {
            if !clock.playing
                && !clock.looping
                && clock.has_range()
                && (clock.current_time_code - clock.end_time_code).abs() < 1e-9
            {
                clock.current_time_code = clock.start_time_code;
            }
            clock.playing = !clock.playing;
        }
        PlaybackRequest::Play => {
            if !clock.looping
                && clock.has_range()
                && (clock.current_time_code - clock.end_time_code).abs() < 1e-9
            {
                clock.current_time_code = clock.start_time_code;
            }
            clock.playing = true;
        }
        PlaybackRequest::Pause => {
            clock.playing = false;
        }
        PlaybackRequest::Stop => {
            clock.stop();
        }
        PlaybackRequest::Scrub(tc) => {
            clock.scrub_to(tc);
        }
        PlaybackRequest::SetLooping(v) => {
            clock.looping = v;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// AnimClipPlayer — per-entity multi-playhead clip playback
// ---------------------------------------------------------------------------

/// One independent playhead: a single clip playing on a single target subtree.
///
/// Unlike the stage-global [`StagePlayback`] (one clock for the whole stage),
/// an entity may carry an [`AnimClipPlayer`] holding several [`ClipPlayhead`]s,
/// each with its own time range, speed, loop flag and — crucially — its own
/// `current_time_code`. Every playhead resamples its own `targets` at its own
/// time code each frame, so two subtrees can play different clips at different
/// rates simultaneously ("multi-playhead sampling"). The clock math is shared
/// with `StagePlayback` through [`integrate_time_code`], so loop/clamp/reverse
/// semantics are identical.
#[derive(Debug, Clone)]
pub struct ClipPlayhead {
    /// Stable label identifying this clip (e.g. a `UsdSkelAnimation` path or a
    /// user-assigned clip name). Playback commands address a playhead by this
    /// label, so it is unique within one [`AnimClipPlayer`].
    pub clip: String,
    pub playing: bool,
    pub current_time_code: f64,
    pub start_time_code: f64,
    pub end_time_code: f64,
    pub time_codes_per_second: f64,
    pub speed: f64,
    pub looping: bool,
    /// Prim subtrees this playhead drives — the `roots` passed to
    /// `sample_viewport_roots(current_time_code, roots)` by the session.
    pub targets: Vec<PrimId>,
    /// Authored policy, retained separately from transport overrides.
    pub declaration: Option<idtx_usd::AnimationClip>,
    pub discovered: bool,
    pub completed: bool,
    /// The most recently completed run, retained across seeks.
    pub completed_run_id: Option<u64>,
    pub stopped: bool,
    pub run_id: u64,
    pub autoplay_started: bool,
    pub error: String,
    pub last_command_sequence: u64,
    pub last_command_applied: Option<bool>,
    pub last_command_error: String,
    pub last_placement_time_code: f64,
    /// Whether this playhead owns its target subtrees' sampling independently of
    /// the stage-global [`StagePlayback`] clock.
    ///
    /// - `true` (an **independent override**): the playhead is advanced and
    ///   resampled on its own clock by [`tick_clip_players`] /
    ///   [`collect_clip_sample_requests`], and the session excludes its
    ///   `targets` from the stage-global resample so the clip pose is not
    ///   clobbered. This is the historical behaviour, so [`ClipPlayhead::new`]
    ///   defaults it to `true`.
    /// - `false` (**following the containing scope**): the playhead exists (so a
    ///   command can later address it by label) but contributes no independent
    ///   sample — its targets keep being driven by the stage clock. Discovery
    ///   hydration ([`hydrate_following_playheads`]) creates playheads in this
    ///   state so that merely cataloguing an animation never freezes the stage.
    ///
    /// A transport command acquires independence ([`ClipPlayhead::acquire`]);
    /// returning to the scope timeline releases it ([`ClipPlayhead::release`]).
    pub independent: bool,
    pub last_sampled_time_code: f64,
    pub needs_initial_sample: bool,
}

impl ClipPlayhead {
    /// Create a paused playhead over `[start, end]` at `tps` time-codes/sec,
    /// positioned at `start`.
    #[must_use]
    pub fn new(clip: impl Into<String>, start: f64, end: f64, tps: f64) -> Self {
        let tps = if tps > 0.0 { tps } else { 24.0 };
        Self {
            clip: clip.into(),
            playing: false,
            current_time_code: start,
            start_time_code: start,
            end_time_code: end,
            time_codes_per_second: tps,
            speed: 1.0,
            looping: true,
            targets: Vec::new(),
            declaration: None,
            discovered: false,
            completed: false,
            completed_run_id: None,
            stopped: true,
            run_id: 0,
            autoplay_started: false,
            error: String::new(),
            last_command_sequence: 0,
            last_command_applied: None,
            last_command_error: String::new(),
            last_placement_time_code: f64::NAN,
            independent: true,
            last_sampled_time_code: f64::NAN,
            needs_initial_sample: true,
        }
    }

    /// Attach the prim subtrees this playhead resamples.
    #[must_use]
    pub fn with_targets(mut self, targets: Vec<PrimId>) -> Self {
        self.targets = targets;
        self
    }

    /// Mark this playhead as **following its containing scope** rather than
    /// owning its targets independently (see [`ClipPlayhead::independent`]).
    /// Builder form used by discovery hydration.
    #[must_use]
    pub fn following(mut self) -> Self {
        self.independent = false;
        self
    }

    /// Acquire independent ownership of this playhead's targets: flip it to an
    /// override driven by its own clock and force a resample so the clip pose
    /// takes effect immediately. Called when a transport command addresses a
    /// playhead that was following its scope.
    pub fn acquire(&mut self) {
        self.independent = true;
        self.needs_initial_sample = true;
    }

    /// Release independent ownership: the playhead stops contributing its own
    /// sample and its targets return to the containing scope's clock. Does not
    /// change `current_time_code` (callers that also want a pose reset call
    /// [`ClipPlayhead::stop`] first).
    pub fn release(&mut self) {
        self.independent = false;
        self.playing = false;
    }

    #[must_use]
    pub fn duration_time_codes(&self) -> f64 {
        (self.end_time_code - self.start_time_code).max(0.0)
    }

    #[must_use]
    pub fn has_range(&self) -> bool {
        self.end_time_code > self.start_time_code
    }

    /// Advance this playhead by `dt_seconds` of real time. No-op when paused or
    /// range-less. Shares the exact integrator used by [`StagePlayback`].
    pub fn advance(&mut self, dt_seconds: f64) {
        if !self.playing || !self.has_range() || !dt_seconds.is_finite() || dt_seconds <= 0.0 || self.speed == 0.0 {
            return;
        }
        let (next, playing) = integrate_time_code(
            self.current_time_code,
            self.start_time_code,
            self.end_time_code,
            self.time_codes_per_second,
            self.speed,
            self.looping,
            dt_seconds,
        );
        self.current_time_code = next;
        self.playing = playing;
    }

    pub fn scrub_to(&mut self, tc: f64) {
        self.current_time_code = if self.has_range() {
            tc.clamp(self.start_time_code, self.end_time_code)
        } else {
            self.start_time_code
        };
    }

    pub fn stop(&mut self) {
        self.playing = false;
        self.current_time_code = self.start_time_code;
        self.completed = false;
        self.stopped = true;
    }

    /// True when a new USD sample is needed this frame (initial sample pending
    /// or the time code has moved since the last sample).
    #[must_use]
    pub fn sample_pending(&self) -> bool {
        if self.needs_initial_sample || self.last_sampled_time_code.is_nan() {
            return true;
        }
        (self.last_sampled_time_code - self.current_time_code).abs() > 1e-9
    }

    pub fn mark_sampled(&mut self) {
        self.last_sampled_time_code = self.current_time_code;
        self.needs_initial_sample = false;
    }
}

/// Per-entity multi-playhead clip player.
///
/// Holds any number of independent [`ClipPlayhead`]s. Attached to an entity by
/// authoring or by a gameplay behavior; ticked each frame by
/// [`tick_clip_players`].
#[derive(Component, Debug, Clone, Default)]
pub struct AnimClipPlayer {
    pub playheads: Vec<ClipPlayhead>,
}

impl AnimClipPlayer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace the playhead with the same `clip` label. Returns a
    /// mutable reference to the stored playhead.
    pub fn set_playhead(&mut self, playhead: ClipPlayhead) -> &mut ClipPlayhead {
        let idx = if let Some(idx) = self.playheads.iter().position(|p| p.clip == playhead.clip) {
            self.playheads[idx] = playhead;
            idx
        } else {
            self.playheads.push(playhead);
            self.playheads.len() - 1
        };
        &mut self.playheads[idx]
    }

    #[must_use]
    pub fn playhead(&self, clip: &str) -> Option<&ClipPlayhead> {
        self.playheads.iter().find(|p| p.clip == clip)
    }

    pub fn playhead_mut(&mut self, clip: &str) -> Option<&mut ClipPlayhead> {
        self.playheads.iter_mut().find(|p| p.clip == clip)
    }

    /// Remove the playhead with the given label. Returns `true` when one was
    /// removed.
    pub fn remove_playhead(&mut self, clip: &str) -> bool {
        let before = self.playheads.len();
        self.playheads.retain(|p| p.clip != clip);
        self.playheads.len() != before
    }
}

/// A resample request emitted by [`collect_clip_sample_requests`]: sample the
/// `targets` subtrees at `time_code`. The session feeds this straight into
/// `sample_viewport_roots(time_code, &targets)`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipSampleRequest {
    pub time_code: f64,
    pub targets: Vec<PrimId>,
}

/// Advance every playhead of every [`AnimClipPlayer`] in the world by
/// `dt_seconds`. Returns `true` if any playhead now has a sample pending, so
/// the caller can skip the resample pass entirely when nothing moved.
pub fn tick_clip_players(world: &mut World, dt_seconds: f64) -> bool {
    let mut any_pending = false;
    let mut events = Vec::new();
    let mut q = world.query::<(Entity, &mut AnimClipPlayer)>();
    for (entity, mut player) in q.iter_mut(world) {
        for ph in &mut player.playheads {
            if !ph.independent || !ph.error.is_empty() { continue; }
            let was_playing = ph.playing;
            ph.advance(dt_seconds);
            if was_playing && !ph.playing && !ph.completed {
                ph.completed = true;
                ph.completed_run_id = Some(ph.run_id);
                events.push(crate::playback::ClipEvent::new(entity, ph, "animation.completed"));
            }
            any_pending |= ph.sample_pending();
        }
    }
    for event in events { crate::playback::queue_event(world, event); }
    any_pending
}

/// Collect one [`ClipSampleRequest`] per playhead that needs a fresh sample
/// this frame (skipping playheads that follow their scope, have no targets, or
/// an empty range).
#[must_use]
pub fn collect_clip_sample_requests(world: &mut World) -> Vec<ClipSampleRequest> {
    let mut reqs = Vec::new();
    let mut q = world.query::<&AnimClipPlayer>();
    for player in q.iter(world) {
        for ph in &player.playheads {
            if !ph.independent || ph.targets.is_empty() || !ph.has_range() || !ph.sample_pending() {
                continue;
            }
            reqs.push(ClipSampleRequest {
                time_code: ph.current_time_code,
                targets: ph.targets.clone(),
            });
        }
    }
    reqs
}

/// Mark every playhead as sampled at its current time code. Called by the
/// session after the resample pass so the next frame only resamples playheads
/// whose time code actually changed.
pub fn mark_clip_players_sampled(world: &mut World) {
    let mut q = world.query::<&mut AnimClipPlayer>();
    for mut player in q.iter_mut(world) {
        for ph in &mut player.playheads {
            ph.mark_sampled();
        }
    }
}

/// Collect the set of [`PrimId`]s that any **independent** playhead currently
/// drives (the union of every independent [`ClipPlayhead::targets`]).
///
/// The session uses this to exclude clip-owned subtrees from the stage-global
/// resample, so an independent clip's pose is never clobbered by the whole-stage
/// or classified-domain sample at the stage time code. Following playheads
/// (which contribute no independent sample) are deliberately excluded — their
/// targets stay driven by the stage clock.
#[must_use]
pub fn collect_independent_clip_target_prims(
    world: &mut World,
) -> std::collections::HashSet<PrimId> {
    let mut set = std::collections::HashSet::new();
    let mut q = world.query::<&AnimClipPlayer>();
    for player in q.iter(world) {
        for ph in &player.playheads {
            if ph.independent && ph.error.is_empty() && ph.has_range() {
                set.extend(ph.targets.iter().copied());
            }
        }
    }
    if set.is_empty() { return set; }
    // Namespace expansion is essential: a global animated descendant can write
    // the same actor even when the playhead targets only its SkelRoot/Xform.
    let roots: Vec<_> = world.iter_entities().filter_map(|e| {
        e.get::<PrimNode>().filter(|n| set.contains(&n.prim)).map(|n| n.path.clone())
    }).collect();
    for entity in world.iter_entities() {
        if let Some(node) = entity.get::<PrimNode>() {
            if roots.iter().any(|r| crate::playback::within(&node.path, r)) { set.insert(node.prim); }
        }
    }
    set
}

// ---------------------------------------------------------------------------
// Build / attach helpers
// ---------------------------------------------------------------------------

/// Collect [`PrimId`]s for all entities that carry an [`AnimatedAttribute`].
///
/// Used by the session to build the roots list for `sample_viewport_roots`.
#[must_use]
pub fn collect_animated_prim_ids(world: &World) -> Vec<PrimId> {
    let mut ids = Vec::new();
    for e in world.iter_entities() {
        let entity = e.id();
        if world.get::<AnimatedAttribute>(entity).is_none() {
            continue;
        }
        if let Some(node) = world.get::<PrimNode>(entity) {
            ids.push(node.prim);
        }
    }
    ids
}

/// Split animated prims into three buckets by the cheapest resample they need:
/// - **`xform_only`**: all tracks are `xformOp.*` attributes → cheap
///   `sample_transforms_only()` path (world-matrix patch, no geometry I/O).
/// - **`skel_pose`**: all tracks drive a `UsdSkel` joint palette (`rotations`,
///   `translations`, `scales`) or are bounds-only metadata (`extent`) →
///   `sample_skeleton_poses()` path (joint-palette resample only; mesh geometry
///   is static so it is not re-projected).
/// - **full**: anything else (animated points, visibility, blend shapes, …) →
///   full `sample_viewport_roots()` resample.
#[must_use]
pub fn classify_animated_prim_ids(world: &World) -> (Vec<PrimId>, Vec<PrimId>, Vec<PrimId>) {
    /// Attributes whose animation does not require re-reading mesh geometry or
    /// materials, so the affected prim can take the cheap skeleton-pose path:
    /// - `rotations`/`translations`/`scales` are `UsdSkelAnimation` joint transforms
    ///   (they change only the skeleton pose).
    /// - `extent` is a prim's bounding-box metadata (e.g. animated on a `SkelRoot`).
    ///   It is not consumed per-frame for render correctness — per-frame skinned
    ///   bounds come from the skin bindings — so the fast path leaves it at its
    ///   hydration value rather than triggering a full geometry re-projection.
    fn is_skel_pose_attr(name: &str) -> bool {
        matches!(name, "rotations" | "translations" | "scales" | "extent")
    }
    let mut xform_only = Vec::new();
    let mut skel_pose = Vec::new();
    let mut full_ids = Vec::new();
    let Some(mut query) = world.try_query::<(&PrimNode, &AnimatedAttribute)>() else {
        return (xform_only, skel_pose, full_ids);
    };
    for (node, anim) in query.iter(world) {
        if anim.tracks.is_empty() {
            full_ids.push(node.prim);
            continue;
        }
        let all_xform = anim.tracks.iter().all(|t| match &t.target {
            TrackTarget::Attribute { attr_name } => attr_name.starts_with("xformOp:"),
            _ => false,
        });
        let all_skel_pose = anim.tracks.iter().all(|t| match &t.target {
            TrackTarget::Attribute { attr_name } => is_skel_pose_attr(attr_name),
            TrackTarget::SkeletalJoint { .. } => true,
            TrackTarget::BlendShape { .. } => false,
        });
        if all_xform {
            xform_only.push(node.prim);
        } else if all_skel_pose {
            skel_pose.push(node.prim);
        } else {
            full_ids.push(node.prim);
        }
    }
    (xform_only, skel_pose, full_ids)
}

/// Build [`AnimatedAttribute`] for a single prim, querying the live stage.
/// Returns `None` when the prim has no time-sampled attributes.
#[must_use]
pub fn build_animated_attribute(
    service: &StageService,
    prim_id: PrimId,
) -> Option<AnimatedAttribute> {
    let names = service.animated_attr_names(prim_id).unwrap_or_default();
    if names.is_empty() {
        return None;
    }
    let mut tracks: Vec<AnimTrack> = Vec::new();
    for name in names {
        let ts: Option<TimeSamples> = service.attr_time_samples(prim_id, &name).unwrap_or(None);
        let Some(ts) = ts else { continue };
        if ts.times.is_empty() {
            continue;
        }
        let (start, end) = ts.time_range().unwrap_or((0.0, 0.0));
        let interpolation = ts.interpolation.map(|i| i.as_token().to_owned());
        tracks.push(AnimTrack {
            name: name.clone(),
            target: TrackTarget::Attribute { attr_name: name },
            value_type: String::new(),
            start_time_code: start,
            end_time_code: end,
            sample_count: ts.count,
            interpolation,
        });
    }
    if tracks.is_empty() {
        None
    } else {
        Some(AnimatedAttribute { tracks })
    }
}

/// Populate [`AnimatedAttribute`] components for every prim in the world in
/// one pass. Called after full topology + viewport hydration.
pub fn populate_all_anim_attributes(world: &mut World, service: &StageService) {
    populate_anim_attributes_impl(world, service, None);
}

/// Subtree-scoped counterpart to [`populate_all_anim_attributes`]: samples the
/// per-attribute time-sample metadata (one FFI call per prim) only for the prims
/// in `scope` — the newly-spawned / retopologised set of a localized topology
/// delta. The whole-stage `populate_all_anim_attributes` is the single biggest
/// unconditional FFI scan on a spawn; scoping it to the spawned subtree avoids
/// re-probing every prim in the stage.
pub fn populate_anim_attributes_scoped(
    world: &mut World,
    service: &StageService,
    scope: &std::collections::HashSet<PrimId>,
) {
    populate_anim_attributes_impl(world, service, Some(scope));
}

fn populate_anim_attributes_impl(
    world: &mut World,
    service: &StageService,
    scope: Option<&std::collections::HashSet<PrimId>>,
) {
    let prim_ids: Vec<(Entity, PrimId)> = world
        .iter_entities()
        .filter_map(|e| {
            let entity = e.id();
            world
                .get::<PrimNode>(entity)
                .filter(|n| !n.is_pseudo_root)
                .filter(|n| scope.is_none_or(|s| s.contains(&n.prim)))
                .map(|n| (entity, n.prim))
        })
        .collect();

    for (entity, prim_id) in prim_ids {
        match build_animated_attribute(service, prim_id) {
            Some(comp) => {
                world.entity_mut(entity).insert(comp);
            }
            None => {
                world.entity_mut(entity).remove::<AnimatedAttribute>();
            }
        }
    }
}

/// Build a [`StageAnimClip`] summary from `world` + stage animation metadata.
///
/// Walks every entity carrying [`AnimatedAttribute`] and aggregates tracks
/// into a flat, path-sorted list.  Stage range + rate come from
/// [`StageService::animation`] so the transport slider bounds match the
/// composed range.
#[must_use]
pub fn build_stage_anim_clip(world: &World, service: &StageService) -> StageAnimClip {
    let anim: StageAnimation = service.animation();

    let mut tracks: Vec<(String, AnimTrack)> = Vec::new();
    for e in world.iter_entities() {
        let entity = e.id();
        let Some(anim_attr) = world.get::<AnimatedAttribute>(entity) else {
            continue;
        };
        let path = world
            .get::<PrimNode>(entity)
            .map(|n| n.path.clone())
            .unwrap_or_default();
        for t in &anim_attr.tracks {
            tracks.push((path.clone(), t.clone()));
        }
    }
    tracks.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));

    let track_start = tracks
        .iter()
        .map(|(_, t)| t.start_time_code)
        .reduce(f64::min);
    let track_end = tracks.iter().map(|(_, t)| t.end_time_code).reduce(f64::max);
    let has_animation = anim.has_time_code_range || !tracks.is_empty();

    let start_time_code = if anim.has_time_code_range {
        track_start.map_or(anim.start_time_code, |s| anim.start_time_code.min(s))
    } else {
        track_start.unwrap_or(anim.start_time_code)
    };
    let end_time_code = if anim.has_time_code_range {
        track_end.map_or(anim.end_time_code, |e| anim.end_time_code.max(e))
    } else {
        track_end.unwrap_or(anim.end_time_code)
    };

    let tps = if anim.time_codes_per_second > 0.0 {
        anim.time_codes_per_second
    } else {
        24.0
    };

    StageAnimClip {
        has_animation,
        start_time_code,
        end_time_code,
        time_codes_per_second: tps,
        track_count: tracks.len(),
        tracks,
    }
}

/// Attach (or refresh) [`StageAnimClip`] + [`StagePlayback`] on `pseudo_root`.
///
/// Idempotent: replaces an existing clip. The playback clock is preserved
/// across re-hydration (`info_changed`) — scrub position and play state
/// survive geometry edits.
pub fn attach_stage_anim_clip(world: &mut World, service: &StageService, pseudo_root: Entity) {
    let clip = build_stage_anim_clip(world, service);
    let has_range = clip.end_time_code > clip.start_time_code;
    let has_animation = clip.has_animation;
    let start_tc = clip.start_time_code;
    let end_tc = clip.end_time_code;
    let tps = if clip.time_codes_per_second > 0.0 {
        clip.time_codes_per_second
    } else {
        24.0
    };

    if let Some(mut existing) = world.get_mut::<StageAnimClip>(pseudo_root) {
        *existing = clip;
    } else {
        world.entity_mut(pseudo_root).insert(clip);
    }

    let should_have_clock = has_animation && has_range;
    if should_have_clock {
        if let Some(mut clock) = world.get_mut::<StagePlayback>(pseudo_root) {
            // Refresh bounds / rate; keep playing state, speed, looping.
            clock.start_time_code = start_tc;
            clock.end_time_code = end_tc;
            clock.time_codes_per_second = tps;
            clock.current_time_code = clock.current_time_code.clamp(start_tc, end_tc);
            clock.needs_initial_sample = true;
        } else {
            world.entity_mut(pseudo_root).insert(StagePlayback {
                start_time_code: start_tc,
                end_time_code: end_tc,
                current_time_code: start_tc,
                time_codes_per_second: tps,
                ..StagePlayback::default()
            });
        }
    } else {
        world.entity_mut(pseudo_root).remove::<StagePlayback>();
    }
}

/// Create or refresh **following** [`ClipPlayhead`]s for every playable entry in
/// `catalog`, so a transport command can later address a discovered animation by
/// its [`crate::animation_catalog::AnimationEntry::label`].
///
/// Hydrated playheads are created in the scope-following state
/// ([`ClipPlayhead::independent`] `== false`): they exist but contribute no
/// independent sample, so merely discovering an animation never freezes the
/// stage at its start pose. An existing playhead with the same label is
/// **preserved** — its transport state and independence survive re-hydration;
/// only its range/targets are refreshed and its time code re-clamped, matching
/// how [`attach_stage_anim_clip`] preserves the stage clock across re-hydration.
pub fn hydrate_following_playheads(
    world: &mut World,
    catalog: &AnimationCatalog,
    default_tps: f64,
) {
    let tps = if default_tps > 0.0 { default_tps } else { 24.0 };

    // Map owner PrimId -> entity once (prims carry a unique PrimId).
    let mut by_prim: std::collections::HashMap<PrimId, Entity> = std::collections::HashMap::new();
    for e in world.iter_entities() {
        let entity = e.id();
        if let Some(node) = world.get::<PrimNode>(entity) {
            by_prim.insert(node.prim, entity);
        }
    }

    for entry in &catalog.entries {
        if !entry.has_range() {
            continue;
        }
        let Some(&entity) = by_prim.get(&entry.owner) else {
            continue;
        };
        let label = entry.label().to_owned();
        let (start, end) = (entry.start_time_code, entry.end_time_code);

        if world.get::<AnimClipPlayer>(entity).is_none() {
            world.entity_mut(entity).insert(AnimClipPlayer::new());
        }
        let mut player = world
            .get_mut::<AnimClipPlayer>(entity)
            .expect("just inserted when absent");

        if let Some(existing) = player.playhead_mut(&label) {
            if existing.declaration.is_some() { continue; }
            // Preserve transport state / independence; refresh bounds + targets.
            let changed = existing.start_time_code != start || existing.end_time_code != end
                || existing.time_codes_per_second != tps || existing.targets != entry.affected;
            existing.start_time_code = start;
            existing.end_time_code = end;
            existing.time_codes_per_second = tps;
            existing.targets = entry.affected.clone();
            existing.current_time_code = existing.current_time_code.clamp(start, end);
            existing.needs_initial_sample |= changed;
        } else {
            let mut head = ClipPlayhead::new(label, start, end, tps)
                    .with_targets(entry.affected.clone())
                    .following();
            head.discovered = true;
            player.set_playhead(head);
        }
    }
    let labels: std::collections::HashSet<_> = catalog.entries.iter()
        .filter(|e| e.has_range()).map(|e| (e.owner, e.label().to_owned())).collect();
    let mut q = world.query::<(&PrimNode, &mut AnimClipPlayer)>();
    for (node, mut player) in q.iter_mut(world) {
        player.playheads.retain(|p| !p.discovered || labels.contains(&(node.prim, p.clip.clone())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use idtx_usd::StageGeneration;

    fn pid(slot: u32) -> PrimId {
        PrimId::from_raw_parts(slot, StageGeneration::from_raw(1))
    }

    fn head(clip: &str) -> ClipPlayhead {
        // 24 tc over [0, 24] at 24 tps → exactly 1 second of playback.
        ClipPlayhead::new(clip, 0.0, 24.0, 24.0)
    }

    #[test]
    fn playhead_advances_forward_at_rate() {
        let mut ph = head("walk");
        ph.playing = true;
        ph.advance(0.5); // 0.5 s * 24 tps * 1.0 = 12 tc
        assert!((ph.current_time_code - 12.0).abs() < 1e-9);
        assert!(ph.playing);
    }

    #[test]
    fn playhead_loops_via_rem_euclid() {
        let mut ph = head("walk");
        ph.playing = true;
        ph.looping = true;
        ph.advance(1.5); // 36 tc over a 24 tc span → wraps to 12
        assert!((ph.current_time_code - 12.0).abs() < 1e-9);
        assert!(ph.playing, "looping clip keeps playing");
    }

    #[test]
    fn playhead_non_looping_clamps_and_stops() {
        let mut ph = head("fire");
        ph.playing = true;
        ph.looping = false;
        ph.advance(2.0); // 48 tc, past the 24 tc end
        assert!((ph.current_time_code - 24.0).abs() < 1e-9);
        assert!(!ph.playing, "non-looping clip stops at the end");
    }

    #[test]
    fn playhead_reverse_clamps_and_stops_at_start() {
        let mut ph = head("rewind");
        ph.playing = true;
        ph.looping = false;
        ph.speed = -1.0;
        ph.current_time_code = 12.0;
        ph.advance(1.0); // -24 tc, past the start
        assert!((ph.current_time_code - 0.0).abs() < 1e-9);
        assert!(!ph.playing);
    }

    #[test]
    fn playhead_matches_stage_playback_integrator() {
        // Parity check: the shared integrator gives ClipPlayhead and
        // StagePlayback identical results for identical inputs.
        let mut ph = head("x");
        ph.playing = true;
        ph.looping = true;
        let mut clock = StagePlayback {
            playing: true,
            current_time_code: 0.0,
            start_time_code: 0.0,
            end_time_code: 24.0,
            time_codes_per_second: 24.0,
            speed: 1.3,
            looping: true,
            last_sampled_time_code: f64::NAN,
            needs_initial_sample: true,
        };
        ph.speed = 1.3;
        for _ in 0..7 {
            ph.advance(0.37);
            clock.advance(0.37);
        }
        assert!((ph.current_time_code - clock.current_time_code).abs() < 1e-9);
    }

    #[test]
    fn sample_pending_tracks_movement() {
        let mut ph = head("walk");
        assert!(ph.sample_pending(), "initial sample always pending");
        ph.mark_sampled();
        assert!(!ph.sample_pending(), "no movement → no resample");
        ph.playing = true;
        ph.advance(0.1);
        assert!(ph.sample_pending(), "time code moved → resample");
    }

    #[test]
    fn scrub_and_stop() {
        let mut ph = head("walk");
        ph.scrub_to(100.0);
        assert!((ph.current_time_code - 24.0).abs() < 1e-9, "clamped to end");
        ph.playing = true;
        ph.stop();
        assert!(!ph.playing);
        assert!(
            (ph.current_time_code - 0.0).abs() < 1e-9,
            "stop resets to start"
        );
    }

    #[test]
    fn player_set_playhead_adds_then_replaces() {
        let mut player = AnimClipPlayer::new();
        player.set_playhead(head("walk"));
        player.set_playhead(head("run"));
        assert_eq!(player.playheads.len(), 2);
        // Same label replaces in place, not appends.
        let mut walk2 = head("walk");
        walk2.speed = 2.0;
        player.set_playhead(walk2);
        assert_eq!(player.playheads.len(), 2);
        assert!((player.playhead("walk").unwrap().speed - 2.0).abs() < 1e-9);
    }

    #[test]
    fn player_remove_playhead() {
        let mut player = AnimClipPlayer::new();
        player.set_playhead(head("walk"));
        assert!(player.remove_playhead("walk"));
        assert!(!player.remove_playhead("walk"));
        assert!(player.playhead("walk").is_none());
    }

    #[test]
    fn tick_advances_all_playheads_independently() {
        let mut world = World::new();
        let mut player = AnimClipPlayer::new();
        let mut fast = head("fast");
        fast.playing = true;
        fast.speed = 2.0;
        let mut slow = head("slow");
        slow.playing = true;
        slow.speed = 1.0;
        player.set_playhead(fast);
        player.set_playhead(slow);
        world.spawn(player);

        let pending = tick_clip_players(&mut world, 0.25); // 0.25 s
        assert!(pending);
        let mut q = world.query::<&AnimClipPlayer>();
        let p = q.iter(&world).next().unwrap();
        // fast: 0.25 * 24 * 2 = 12; slow: 0.25 * 24 * 1 = 6.
        assert!((p.playhead("fast").unwrap().current_time_code - 12.0).abs() < 1e-9);
        assert!((p.playhead("slow").unwrap().current_time_code - 6.0).abs() < 1e-9);
    }

    #[test]
    fn collect_requests_only_for_targeted_moving_playheads() {
        let mut world = World::new();
        let mut player = AnimClipPlayer::new();

        // Targeted + playing → should produce a request.
        let mut a = head("a").with_targets(vec![pid(1), pid(2)]);
        a.playing = true;
        // No targets → never produces a request even though it moves.
        let mut b = head("b");
        b.playing = true;
        player.set_playhead(a);
        player.set_playhead(b);
        world.spawn(player);

        tick_clip_players(&mut world, 0.5);
        let reqs = collect_clip_sample_requests(&mut world);
        assert_eq!(
            reqs.len(),
            1,
            "only the targeted playhead requests a sample"
        );
        assert!((reqs[0].time_code - 12.0).abs() < 1e-9);
        assert_eq!(reqs[0].targets, vec![pid(1), pid(2)]);

        // After marking sampled, a stationary (paused) frame produces nothing.
        mark_clip_players_sampled(&mut world);
        {
            let mut q = world.query::<&mut AnimClipPlayer>();
            for mut p in q.iter_mut(&mut world) {
                for ph in &mut p.playheads {
                    ph.playing = false;
                }
            }
        }
        tick_clip_players(&mut world, 0.5);
        assert!(collect_clip_sample_requests(&mut world).is_empty());
    }

    #[test]
    fn following_playhead_produces_no_request_until_acquired() {
        let mut world = World::new();
        let mut player = AnimClipPlayer::new();
        // A hydrated, scope-following playhead: targeted + playing but NOT
        // independent → must contribute no independent sample.
        let mut following = head("open").with_targets(vec![pid(1)]).following();
        following.playing = true;
        player.set_playhead(following);
        world.spawn(player);

        tick_clip_players(&mut world, 0.5);
        assert!(
            collect_clip_sample_requests(&mut world).is_empty(),
            "a following playhead never requests its own sample"
        );
        assert!(
            collect_independent_clip_target_prims(&mut world).is_empty(),
            "a following playhead's targets stay driven by the stage clock"
        );

        // A transport command acquires independence → now it owns its targets.
        {
            let mut q = world.query::<&mut AnimClipPlayer>();
            let mut p = q.iter_mut(&mut world).next().unwrap();
            p.playhead_mut("open").unwrap().acquire();
        }
        tick_clip_players(&mut world, 0.5);
        assert_eq!(
            collect_clip_sample_requests(&mut world).len(),
            1,
            "once acquired, the playhead samples independently"
        );
        let owned = collect_independent_clip_target_prims(&mut world);
        assert!(owned.contains(&pid(1)), "acquired targets are clip-owned");
    }

    #[test]
    fn release_returns_targets_to_scope() {
        let mut world = World::new();
        let mut player = AnimClipPlayer::new();
        let a = head("open").with_targets(vec![pid(1)]); // independent by default
        player.set_playhead(a);
        world.spawn(player);

        assert!(collect_independent_clip_target_prims(&mut world).contains(&pid(1)));
        {
            let mut q = world.query::<&mut AnimClipPlayer>();
            let mut p = q.iter_mut(&mut world).next().unwrap();
            p.playhead_mut("open").unwrap().release();
        }
        assert!(
            collect_independent_clip_target_prims(&mut world).is_empty(),
            "released targets are no longer clip-owned"
        );
    }

    #[test]
    fn new_playheads_are_independent_by_default() {
        let ph = head("walk");
        assert!(ph.independent, "ClipPlayhead::new preserves legacy behaviour");
        assert!(!ph.following().independent);
    }

    #[test]
    fn hydrate_creates_following_playheads_and_preserves_acquired() {
        use idtx_usd::PrimState;
        let mut world = World::new();
        let node = PrimNode {
            prim: pid(1),
            path: "/World/Door".to_owned(),
            parent: None,
            name: "Door".to_owned(),
            type_name: "Xform".to_owned(),
            state: PrimState {
                active: true,
                defined: true,
                loaded: true,
            },
            is_pseudo_root: false,
            local_layer_identifier: None,
        };
        let anim = AnimatedAttribute {
            tracks: vec![AnimTrack {
                name: "xformOp:translate".to_owned(),
                target: TrackTarget::Attribute {
                    attr_name: "xformOp:translate".to_owned(),
                },
                value_type: String::new(),
                start_time_code: 0.0,
                end_time_code: 24.0,
                sample_count: 2,
                interpolation: None,
            }],
        };
        let e = world.spawn((node, anim)).id();

        let catalog = AnimationCatalog::build(&world);
        hydrate_following_playheads(&mut world, &catalog, 24.0);

        {
            let player = world.get::<AnimClipPlayer>(e).expect("player hydrated");
            let ph = player.playhead("/World/Door").expect("labelled by path");
            assert!(!ph.independent, "hydrated playheads follow their scope");
            assert_eq!(ph.targets, vec![pid(1)]);
            assert!((ph.end_time_code - 24.0).abs() < 1e-9);
        }

        // Acquire it (a command controlled it), then re-hydrate: independence
        // and play state must survive.
        {
            let mut player = world.get_mut::<AnimClipPlayer>(e).unwrap();
            let ph = player.playhead_mut("/World/Door").unwrap();
            ph.acquire();
            ph.playing = true;
        }
        let catalog = AnimationCatalog::build(&world);
        hydrate_following_playheads(&mut world, &catalog, 24.0);
        let player = world.get::<AnimClipPlayer>(e).unwrap();
        let ph = player.playhead("/World/Door").unwrap();
        assert!(ph.independent, "re-hydration preserves an acquired override");
        assert!(ph.playing);
        assert_eq!(player.playheads.len(), 1, "no duplicate playhead");
    }
}
