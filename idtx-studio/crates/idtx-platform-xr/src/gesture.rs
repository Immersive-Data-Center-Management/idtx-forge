//! Hand-gesture recognition (Phase 10).
//!
//! The `OpenXR` runtime provides raw joint poses only — no semantic gestures.
//! This module interprets the 26-joint [`HandPose`] stream produced by
//! `XR_EXT_hand_tracking` into named [`GestureEvent`]s (pinch, fist, swipe, …)
//! that the input layer can bind to actions alongside keyboard and controller
//! input.
//!
//! # Architecture
//!
//! Recognition runs on its own thread so pattern matching never stalls the
//! render/frame loop:
//!
//! ```text
//! frame loop ── HandSnapshot ──▶ [sync_channel(8)] ──▶ GestureWorker thread
//!                                                          │ per-hand detectors
//! frame loop ◀── GestureEvent ◀── [sync_channel(64)] ◀────┘
//! ```
//!
//! The worker owns one [`SimpleGestureDetector`] (stateless, single-frame
//! heuristics) and one [`TemporalGestureDetector`] per hand (a sliding window
//! over wrist motion for swipes / push / circle). Continuous gestures emit
//! [`GesturePhase::Started`] / [`Held`](GesturePhase::Held) /
//! [`Ended`](GesturePhase::Ended) edges; discrete motion gestures fire a single
//! `Started` with a refractory cooldown to avoid repeats.
//!
//! # Feature gating
//!
//! Behind `gesture-recognition`. The extra timing instrumentation is behind the
//! additional `gesture-metrics` sub-feature.

use std::collections::VecDeque;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::thread::JoinHandle;

use glam::{Vec2, Vec3};

use crate::hand::HandPose;

// ── Joint indices (mirror the XR_HAND_JOINT_*_EXT ordinals) ─────────────────

/// Palm centre (hierarchy root).
pub const PALM: usize = 0;
/// Wrist.
pub const WRIST: usize = 1;
/// Thumb tip.
pub const THUMB_TIP: usize = 5;
/// Thumb metacarpal (base knuckle) — `THUMB_TIP - this` is the thumb direction.
pub const THUMB_METACARPAL: usize = 2;
/// Index-finger metacarpophalangeal (base knuckle).
pub const INDEX_PROXIMAL: usize = 7;
/// Index-finger tip.
pub const INDEX_TIP: usize = 10;
/// Middle-finger tip.
pub const MIDDLE_TIP: usize = 15;
/// Ring-finger tip.
pub const RING_TIP: usize = 20;
/// Little-finger tip.
pub const LITTLE_TIP: usize = 25;

/// The four non-thumb fingertip indices, used by fist / open-hand heuristics.
const FINGER_TIPS: [usize; 4] = [INDEX_TIP, MIDDLE_TIP, RING_TIP, LITTLE_TIP];

// ── Public event vocabulary ─────────────────────────────────────────────────

/// Which hand produced a gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hand {
    /// Left hand.
    Left,
    /// Right hand.
    Right,
}

impl Hand {
    /// Lowercase source-string suffix (`"left"` / `"right"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Hand::Left => "left",
            Hand::Right => "right",
        }
    }
}

/// Lifecycle edge of a gesture.
///
/// Continuous gestures (pinch, fist, …) transition `Started → Held* → Ended`.
/// Discrete gestures (swipes, push, circle) fire a single `Started`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GesturePhase {
    /// The gesture just became active this frame.
    Started,
    /// The gesture remained active from the previous frame.
    Held,
    /// The gesture stopped being active this frame.
    Ended,
}

/// A recognised gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureKind {
    /// Thumb tip and index tip touching.
    Pinch,
    /// All fingers curled toward the palm.
    Fist,
    /// Index extended, other fingers curled.
    Point,
    /// All fingers extended and spread.
    OpenHand,
    /// Thumb up, fingers curled.
    ThumbsUp,
    /// Rapid wrist motion toward -X.
    SwipeLeft,
    /// Rapid wrist motion toward +X.
    SwipeRight,
    /// Rapid wrist motion toward +Y.
    SwipeUp,
    /// Rapid wrist motion toward -Y.
    SwipeDown,
    /// Rapid wrist motion toward -Z (away from the body / forward).
    PushForward,
    /// Wrist tracing a closed loop in the XY plane.
    Circle,
}

impl GestureKind {
    /// Stable identifier used to build the input source string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            GestureKind::Pinch => "pinch",
            GestureKind::Fist => "fist",
            GestureKind::Point => "point",
            GestureKind::OpenHand => "open_hand",
            GestureKind::ThumbsUp => "thumbs_up",
            GestureKind::SwipeLeft => "swipe_left",
            GestureKind::SwipeRight => "swipe_right",
            GestureKind::SwipeUp => "swipe_up",
            GestureKind::SwipeDown => "swipe_down",
            GestureKind::PushForward => "push_forward",
            GestureKind::Circle => "circle",
        }
    }

    /// Discrete gestures are momentary motions that fire once; continuous
    /// gestures are held poses that report `Started`/`Held`/`Ended` edges.
    #[must_use]
    pub fn is_discrete(self) -> bool {
        matches!(
            self,
            GestureKind::SwipeLeft
                | GestureKind::SwipeRight
                | GestureKind::SwipeUp
                | GestureKind::SwipeDown
                | GestureKind::PushForward
                | GestureKind::Circle
        )
    }
}

/// A recognised gesture event emitted by the worker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GestureEvent {
    /// Which hand produced it.
    pub hand: Hand,
    /// What was recognised.
    pub kind: GestureKind,
    /// Lifecycle edge.
    pub phase: GesturePhase,
    /// Recogniser confidence in `[0, 1]`.
    pub confidence: f32,
    /// Predicted display timestamp (seconds) of the pose that triggered it.
    pub timestamp: f64,
}

impl GestureEvent {
    /// The `Device/Control` source string for the input router, e.g.
    /// `"Gesture/pinch/left"`. Bind actions to these in `input_map.toml`.
    #[must_use]
    pub fn source_string(&self) -> String {
        format!("Gesture/{}/{}", self.kind.as_str(), self.hand.as_str())
    }
}

/// One frame of located hand poses fed to the worker.
#[derive(Debug, Clone)]
pub struct HandSnapshot {
    /// Left-hand pose, or `None` when untracked this frame.
    pub left: Option<HandPose>,
    /// Right-hand pose, or `None` when untracked this frame.
    pub right: Option<HandPose>,
    /// Monotonic frame counter (diagnostics only).
    pub frame_index: u64,
}

// ── Configuration ───────────────────────────────────────────────────────────

/// Thresholds for the single-frame pose heuristics (metres).
#[derive(Debug, Clone, Copy)]
pub struct SimpleGestureConfig {
    /// Max thumb-tip↔index-tip distance for a pinch.
    pub pinch_distance: f32,
    /// Max fingertip↔palm distance for a finger to count as "curled".
    pub curl_distance: f32,
    /// Min fingertip↔palm distance for a finger to count as "extended".
    pub extend_distance: f32,
    /// Min thumb-tip height above the wrist for a thumbs-up.
    pub thumb_up_height: f32,
}

impl Default for SimpleGestureConfig {
    fn default() -> Self {
        // Calibrated against real device hand-tracking (Vision Pro ARKit; the
        // OpenXR 26-joint model is the same scale): fingertip↔PALM(centre)
        // distances are larger than a naive guess — a *curled* fingertip sits
        // ~0.08–0.10 m from the palm centre and an *extended* one ~0.15–0.20 m,
        // so the curl/extend boundary lives around 0.12–0.13 m, not 0.06/0.08.
        // (The earlier 0.06/0.08 made `Point` impossible: curled fingers never
        // reached within 6 cm of the palm centre, so middle/ring/little never
        // read as curled.) Verified with the on-device `point R:` diagnostic.
        Self {
            pinch_distance: 0.025,
            curl_distance: 0.12,
            extend_distance: 0.13,
            thumb_up_height: 0.05,
        }
    }
}

/// Thresholds for the temporal (wrist-motion) detectors.
#[derive(Debug, Clone, Copy)]
pub struct TemporalGestureConfig {
    /// How long wrist samples are retained (seconds).
    pub history_window: f64,
    /// Time window a swipe must complete within (seconds).
    pub swipe_window: f64,
    /// Min axis-aligned displacement for a swipe (metres).
    pub swipe_distance: f32,
    /// Time window a push must complete within (seconds).
    pub push_window: f64,
    /// Min -Z displacement for a push-forward (metres).
    pub push_distance: f32,
    /// Time window a circle must complete within (seconds).
    pub circle_window: f64,
    /// Min traced arc length for a circle (metres).
    pub circle_arc: f32,
    /// Min swept angle for a circle (radians).
    pub circle_angle: f32,
    /// Refractory period between repeat fires of the same discrete gesture.
    pub discrete_cooldown: f64,
}

impl Default for TemporalGestureConfig {
    fn default() -> Self {
        Self {
            history_window: 1.0,
            swipe_window: 0.35,
            swipe_distance: 0.15,
            push_window: 0.4,
            push_distance: 0.12,
            circle_window: 1.0,
            circle_arc: 0.30,
            circle_angle: 4.712_389, // 270°
            discrete_cooldown: 0.5,
        }
    }
}

/// Combined recogniser configuration + channel bounds.
#[derive(Debug, Clone, Copy)]
pub struct GestureConfig {
    /// Single-frame pose thresholds.
    pub simple: SimpleGestureConfig,
    /// Wrist-motion thresholds.
    pub temporal: TemporalGestureConfig,
    /// Capacity of the joint (frame-loop → worker) channel.
    pub joint_capacity: usize,
    /// Capacity of the event (worker → frame-loop) channel.
    pub event_capacity: usize,
}

impl Default for GestureConfig {
    fn default() -> Self {
        Self {
            simple: SimpleGestureConfig::default(),
            temporal: TemporalGestureConfig::default(),
            joint_capacity: 8,
            event_capacity: 64,
        }
    }
}

// ── Simple (single-frame) detector ──────────────────────────────────────────

fn dist(pose: &HandPose, a: usize, b: usize) -> f32 {
    pose.joints[a].position.distance(pose.joints[b].position)
}

/// Stateless single-frame pose classifier.
#[derive(Debug, Clone, Copy, Default)]
pub struct SimpleGestureDetector {
    cfg: SimpleGestureConfig,
}

impl SimpleGestureDetector {
    /// Build with explicit thresholds.
    #[must_use]
    pub fn new(cfg: SimpleGestureConfig) -> Self {
        Self { cfg }
    }

    /// Classify a single pose, appending `(kind, confidence)` for every
    /// continuous gesture currently held.
    ///
    /// Gestures are checked in priority order; a matched pinch suppresses the
    /// more general open-hand / point checks so only one dominant pose fires.
    pub fn detect(&self, pose: &HandPose, out: &mut Vec<(GestureKind, f32)>) {
        let cfg = &self.cfg;

        let pinch_d = dist(pose, THUMB_TIP, INDEX_TIP);
        let pinch = pinch_d < cfg.pinch_distance;
        if pinch {
            out.push((GestureKind::Pinch, norm_below(pinch_d, cfg.pinch_distance)));
        }

        // Per-finger curl / extend state relative to the palm.
        let curled = |tip: usize| dist(pose, tip, PALM) < cfg.curl_distance;
        let extended = |tip: usize| dist(pose, tip, PALM) > cfg.extend_distance;

        let all_curled = FINGER_TIPS.iter().all(|&t| curled(t));
        let all_extended = FINGER_TIPS.iter().all(|&t| extended(t));

        if all_curled {
            let conf = FINGER_TIPS
                .iter()
                .map(|&t| norm_below(dist(pose, t, PALM), cfg.curl_distance))
                .sum::<f32>()
                / 4.0;
            out.push((GestureKind::Fist, conf));

            // Thumbs-up: fist of fingers plus a raised thumb.
            let thumb_h = pose.joints[THUMB_TIP].position.y - pose.joints[WRIST].position.y;
            if thumb_h > cfg.thumb_up_height {
                out.push((GestureKind::ThumbsUp, norm_above(thumb_h, cfg.thumb_up_height)));
            }
        } else if all_extended && !pinch {
            let conf = FINGER_TIPS
                .iter()
                .map(|&t| norm_above(dist(pose, t, PALM), cfg.extend_distance))
                .sum::<f32>()
                / 4.0;
            out.push((GestureKind::OpenHand, conf));
        } else if !pinch
            && extended(INDEX_TIP)
            && curled(MIDDLE_TIP)
            && curled(RING_TIP)
            && curled(LITTLE_TIP)
        {
            out.push((
                GestureKind::Point,
                norm_above(dist(pose, INDEX_TIP, PALM), cfg.extend_distance),
            ));
        }
    }
}

/// Per-frame diagnostic for the [`GestureKind::Point`] pose: the measured
/// fingertip↔palm distances (and the thumb↔index pinch distance) alongside the
/// thresholds `Point` detection compares them against. Lets a field session log
/// *why* a point did or did not register — e.g. the index not extended far
/// enough, or the middle finger not curled tightly enough — so the thresholds in
/// [`SimpleGestureConfig`] can be retuned for a given device's hand scale.
///
/// Mirrors the `Point` branch of [`SimpleGestureDetector::detect`]: `Point`
/// fires when the index is extended, the middle/ring/little are curled, and the
/// hand is not pinching. Produce one with [`point_diagnostics`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointDiagnostics {
    /// Index-tip↔palm distance (metres); must exceed `extend_threshold`.
    pub index_extend: f32,
    /// Middle-tip↔palm distance; must be below `curl_threshold`.
    pub middle_curl: f32,
    /// Ring-tip↔palm distance; must be below `curl_threshold`.
    pub ring_curl: f32,
    /// Little-tip↔palm distance; must be below `curl_threshold`.
    pub little_curl: f32,
    /// Thumb-tip↔index-tip distance; must be ≥ `pinch_threshold` (else a pinch
    /// suppresses `Point`).
    pub pinch: f32,
    /// The "extended" threshold a fingertip↔palm distance must exceed.
    pub extend_threshold: f32,
    /// The "curled" threshold a fingertip↔palm distance must stay below.
    pub curl_threshold: f32,
    /// The pinch threshold the thumb↔index distance must stay at or above.
    pub pinch_threshold: f32,
}

impl PointDiagnostics {
    /// Whether the index finger reads as extended.
    #[must_use]
    pub fn index_extended(&self) -> bool {
        self.index_extend > self.extend_threshold
    }
    /// Whether the middle finger reads as curled.
    #[must_use]
    pub fn middle_curled(&self) -> bool {
        self.middle_curl < self.curl_threshold
    }
    /// Whether the ring finger reads as curled.
    #[must_use]
    pub fn ring_curled(&self) -> bool {
        self.ring_curl < self.curl_threshold
    }
    /// Whether the little finger reads as curled.
    #[must_use]
    pub fn little_curled(&self) -> bool {
        self.little_curl < self.curl_threshold
    }
    /// Whether the hand reads as pinching (which suppresses `Point`).
    #[must_use]
    pub fn pinching(&self) -> bool {
        self.pinch < self.pinch_threshold
    }
    /// Whether all conditions for `Point` are met this frame.
    #[must_use]
    pub fn is_point(&self) -> bool {
        !self.pinching()
            && self.index_extended()
            && self.middle_curled()
            && self.ring_curled()
            && self.little_curled()
    }
}

impl std::fmt::Display for PointDiagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "idx {:.3}>{:.3}={} mid {:.3}<{:.3}={} ring {:.3}<{:.3}={} lil {:.3}<{:.3}={} pinch {:.3}>={:.3}={} => POINT={}",
            self.index_extend,
            self.extend_threshold,
            u8::from(self.index_extended()),
            self.middle_curl,
            self.curl_threshold,
            u8::from(self.middle_curled()),
            self.ring_curl,
            self.curl_threshold,
            u8::from(self.ring_curled()),
            self.little_curl,
            self.curl_threshold,
            u8::from(self.little_curled()),
            self.pinch,
            self.pinch_threshold,
            u8::from(!self.pinching()),
            u8::from(self.is_point()),
        )
    }
}

/// Measure the [`PointDiagnostics`] for `pose` against `cfg` (the same
/// single-frame thresholds [`SimpleGestureDetector`] uses). Pure; cheap.
#[must_use]
pub fn point_diagnostics(pose: &HandPose, cfg: &SimpleGestureConfig) -> PointDiagnostics {
    PointDiagnostics {
        index_extend: dist(pose, INDEX_TIP, PALM),
        middle_curl: dist(pose, MIDDLE_TIP, PALM),
        ring_curl: dist(pose, RING_TIP, PALM),
        little_curl: dist(pose, LITTLE_TIP, PALM),
        pinch: dist(pose, THUMB_TIP, INDEX_TIP),
        extend_threshold: cfg.extend_distance,
        curl_threshold: cfg.curl_distance,
        pinch_threshold: cfg.pinch_distance,
    }
}

/// Cosine of the angle between the thumb direction (`THUMB_TIP -
/// THUMB_METACARPAL`) and the index direction (`INDEX_TIP - INDEX_PROXIMAL`):
/// ~0 when the thumb is cocked perpendicular/up, ~1 when it lies alongside the
/// index. `None` on a degenerate bone. The diagnostic twin of the teleport
/// behavior's thumb-snap commit signal — log it to tune the arm/fire thresholds.
#[must_use]
pub fn thumb_index_cos(pose: &HandPose) -> Option<f32> {
    let thumb = pose.joints[THUMB_TIP].position - pose.joints[THUMB_METACARPAL].position;
    let index = pose.joints[INDEX_TIP].position - pose.joints[INDEX_PROXIMAL].position;
    if thumb.length_squared() < 1e-8 || index.length_squared() < 1e-8 {
        return None;
    }
    Some(thumb.normalize().dot(index.normalize()))
}

/// Distance from the thumb tip to the index base knuckle (`INDEX_PROXIMAL`),
/// metres — the fist-teleport behavior's hand-relative thumb-snap signal (large
/// when the thumb is raised away from the fist, small when snapped onto it). Log
/// it to tune the arm/fire gap thresholds.
#[must_use]
pub fn thumb_index_gap(pose: &HandPose) -> f32 {
    (pose.joints[THUMB_TIP].position - pose.joints[INDEX_PROXIMAL].position).length()
}

/// Confidence for "value below threshold": 1 at 0, 0 at `threshold`.
fn norm_below(value: f32, threshold: f32) -> f32 {
    (1.0 - value / threshold).clamp(0.0, 1.0)
}

/// Confidence for "value above threshold": saturates one threshold past it.
fn norm_above(value: f32, threshold: f32) -> f32 {
    ((value - threshold) / threshold).clamp(0.0, 1.0)
}

// ── Temporal (wrist-motion) detector ────────────────────────────────────────

/// Sliding-window detector over wrist position for motion gestures.
#[derive(Debug, Clone)]
pub struct TemporalGestureDetector {
    cfg: TemporalGestureConfig,
    /// `(wrist_position, timestamp)` samples, oldest first.
    samples: VecDeque<(Vec3, f64)>,
}

impl TemporalGestureDetector {
    /// Build with explicit thresholds.
    #[must_use]
    pub fn new(cfg: TemporalGestureConfig) -> Self {
        Self {
            cfg,
            samples: VecDeque::with_capacity(128),
        }
    }

    /// Drop all buffered history (call when the hand becomes untracked).
    pub fn reset(&mut self) {
        self.samples.clear();
    }

    /// Record one wrist sample and prune stale history.
    pub fn push(&mut self, pose: &HandPose) {
        let pos = pose.joints[WRIST].position;
        let t = pose.timestamp;
        self.samples.push_back((pos, t));
        while let Some(&(_, t0)) = self.samples.front() {
            if t - t0 > self.cfg.history_window {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// Detect discrete motion gestures over the current window.
    ///
    /// Returns at most one swipe/push (the dominant axis) plus a circle when a
    /// full loop is present. `now` is the latest sample timestamp.
    #[must_use]
    pub fn detect(&self, now: f64) -> Vec<(GestureKind, f32)> {
        let mut out = Vec::new();
        let cfg = &self.cfg;

        // Displacement over the swipe window.
        if let Some(delta) = self.displacement_within(now, cfg.swipe_window) {
            let ax = delta.x.abs();
            let ay = delta.y.abs();
            let az = delta.z.abs();
            if ax >= ay && ax >= az && ax > cfg.swipe_distance {
                let kind = if delta.x > 0.0 {
                    GestureKind::SwipeRight
                } else {
                    GestureKind::SwipeLeft
                };
                out.push((kind, norm_above(ax, cfg.swipe_distance)));
            } else if ay >= ax && ay >= az && ay > cfg.swipe_distance {
                let kind = if delta.y > 0.0 {
                    GestureKind::SwipeUp
                } else {
                    GestureKind::SwipeDown
                };
                out.push((kind, norm_above(ay, cfg.swipe_distance)));
            }
        }

        // Push-forward: motion toward -Z within its own window.
        if let Some(delta) = self.displacement_within(now, cfg.push_window)
            && delta.z < -cfg.push_distance
        {
            out.push((GestureKind::PushForward, norm_above(-delta.z, cfg.push_distance)));
        }

        // Circle: arc length + swept angle over the circle window.
        if let Some((arc, angle)) = self.arc_within(now, cfg.circle_window)
            && arc > cfg.circle_arc
            && angle > cfg.circle_angle
        {
            out.push((GestureKind::Circle, norm_above(angle, cfg.circle_angle)));
        }

        out
    }

    /// Net displacement between the first sample within `window` and the last.
    fn displacement_within(&self, now: f64, window: f64) -> Option<Vec3> {
        let last = self.samples.back()?.0;
        let first = self
            .samples
            .iter()
            .find(|&&(_, t)| now - t <= window)
            .map(|&(p, _)| p)?;
        Some(last - first)
    }

    /// Traced arc length and total swept angle (XY plane, about the window
    /// centroid) over `window`.
    fn arc_within(&self, now: f64, window: f64) -> Option<(f32, f32)> {
        let pts: Vec<Vec3> = self
            .samples
            .iter()
            .filter(|&&(_, t)| now - t <= window)
            .map(|&(p, _)| p)
            .collect();
        if pts.len() < 8 {
            return None;
        }

        let count = u16::try_from(pts.len()).unwrap_or(u16::MAX);
        let centroid = pts.iter().copied().sum::<Vec3>() / f32::from(count);

        let mut arc = 0.0f32;
        let mut angle = 0.0f32;
        let mut prev_dir: Option<Vec2> = None;
        for w in pts.windows(2) {
            arc += w[0].distance(w[1]);
            let dir = Vec2::new(w[1].x - centroid.x, w[1].y - centroid.y);
            if dir.length_squared() > 1e-8 {
                if let Some(pd) = prev_dir {
                    angle += pd.angle_to(dir).abs();
                }
                prev_dir = Some(dir);
            }
        }
        Some((arc, angle))
    }
}

// ── Per-hand state + edge detection ─────────────────────────────────────────

/// Tracks per-hand active-gesture state to derive `Started`/`Held`/`Ended`.
#[derive(Debug)]
struct HandState {
    hand: Hand,
    simple: SimpleGestureDetector,
    temporal: TemporalGestureDetector,
    /// Continuous gestures active last frame → their confidence.
    active: Vec<(GestureKind, f32)>,
    /// Last fire time per discrete gesture (for the cooldown).
    last_discrete: Vec<(GestureKind, f64)>,
    /// Discrete gestures that emitted a `Started` last frame and still owe a
    /// `Ended` this frame — discrete gestures are momentary one-frame pulses,
    /// not held states, so the consumer must see them release.
    discrete_pending: Vec<GestureKind>,
}

impl HandState {
    fn new(hand: Hand, cfg: &GestureConfig) -> Self {
        Self {
            hand,
            simple: SimpleGestureDetector::new(cfg.simple),
            temporal: TemporalGestureDetector::new(cfg.temporal),
            active: Vec::new(),
            last_discrete: Vec::new(),
            discrete_pending: Vec::new(),
        }
    }

    /// Advance one frame for this hand, emitting the resulting events.
    fn update(&mut self, pose: Option<&HandPose>, cooldown: f64, out: &mut Vec<GestureEvent>) {
        let Some(pose) = pose else {
            // Hand lost: end everything and clear temporal history.
            for &(kind, _) in &self.active {
                out.push(GestureEvent {
                    hand: self.hand,
                    kind,
                    phase: GesturePhase::Ended,
                    confidence: 0.0,
                    timestamp: 0.0,
                });
            }
            // Release any discrete pulse that fired last frame but never got its
            // closing edge, so it does not stay pressed on the consumer.
            for &kind in &self.discrete_pending {
                out.push(GestureEvent {
                    hand: self.hand,
                    kind,
                    phase: GesturePhase::Ended,
                    confidence: 0.0,
                    timestamp: 0.0,
                });
            }
            self.discrete_pending.clear();
            self.active.clear();
            self.temporal.reset();
            return;
        };

        let ts = pose.timestamp;

        // Continuous gestures: diff this frame's active set against the last.
        let mut now_active: Vec<(GestureKind, f32)> = Vec::new();
        self.simple.detect(pose, &mut now_active);

        for &(kind, conf) in &now_active {
            let was = self.active.iter().any(|&(k, _)| k == kind);
            out.push(GestureEvent {
                hand: self.hand,
                kind,
                phase: if was { GesturePhase::Held } else { GesturePhase::Started },
                confidence: conf,
                timestamp: ts,
            });
        }
        for &(kind, _) in &self.active {
            if !now_active.iter().any(|&(k, _)| k == kind) {
                out.push(GestureEvent {
                    hand: self.hand,
                    kind,
                    phase: GesturePhase::Ended,
                    confidence: 0.0,
                    timestamp: ts,
                });
            }
        }
        self.active = now_active;

        // Discrete motion gestures are momentary pulses: a gesture fired last
        // frame emitted a `Started`, so emit its matching `Ended` now before
        // detecting fresh ones. The cooldown guarantees the same kind cannot
        // re-fire on this very frame, so a release never races its own press.
        for kind in self.discrete_pending.drain(..) {
            out.push(GestureEvent {
                hand: self.hand,
                kind,
                phase: GesturePhase::Ended,
                confidence: 0.0,
                timestamp: ts,
            });
        }

        // Discrete motion gestures: fire once per cooldown window.
        self.temporal.push(pose);
        for (kind, conf) in self.temporal.detect(ts) {
            let last = self
                .last_discrete
                .iter_mut()
                .find(|(k, _)| *k == kind);
            let ready = match &last {
                Some((_, t)) => ts - *t > cooldown,
                None => true,
            };
            if ready {
                match last {
                    Some(slot) => slot.1 = ts,
                    None => self.last_discrete.push((kind, ts)),
                }
                out.push(GestureEvent {
                    hand: self.hand,
                    kind,
                    phase: GesturePhase::Started,
                    confidence: conf,
                    timestamp: ts,
                });
                // Owe a closing `Ended` on the next frame (one-frame pulse).
                self.discrete_pending.push(kind);
            }
        }
    }
}

// ── Worker + channel handle ─────────────────────────────────────────────────

/// The frame-loop side of a running [`GestureWorker`].
///
/// Push per-frame hand poses with [`Self::send`] and drain recognised events
/// with [`Self::try_recv`]. Dropping the link disconnects the joint channel,
/// which stops the worker thread.
#[derive(Debug)]
pub struct GestureLink {
    joint_tx: SyncSender<HandSnapshot>,
    event_rx: Receiver<GestureEvent>,
    handle: Option<JoinHandle<()>>,
}

impl GestureLink {
    /// Send one frame of hand poses to the worker. Non-blocking: if the worker
    /// is behind (channel full), the snapshot is dropped rather than stalling
    /// the render loop. Returns `false` when the worker has stopped.
    #[must_use]
    pub fn send(&self, snap: HandSnapshot) -> bool {
        match self.joint_tx.try_send(snap) {
            Ok(()) | Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Pop the next recognised event, or `None` when none are queued.
    #[must_use]
    pub fn try_recv(&self) -> Option<GestureEvent> {
        self.event_rx.try_recv().ok()
    }

    /// Drain all queued events into a vector.
    #[must_use]
    pub fn drain(&self) -> Vec<GestureEvent> {
        let mut out = Vec::new();
        while let Some(e) = self.try_recv() {
            out.push(e);
        }
        out
    }
}

impl Drop for GestureLink {
    fn drop(&mut self) {
        // Wake both possible blocking operations before joining: recv on an
        // empty input queue and send of a lifecycle edge to a full output queue.
        let (_, dead) = sync_channel::<GestureEvent>(0);
        drop(std::mem::replace(&mut self.event_rx, dead));
        let (dead, _) = sync_channel::<HandSnapshot>(0);
        drop(std::mem::replace(&mut self.joint_tx, dead));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Spawns and owns the recognition thread.
#[derive(Debug)]
pub struct GestureWorker;

impl GestureWorker {
    /// Spawn the recognition thread and return the frame-loop [`GestureLink`].
    ///
    /// The thread runs until the returned link is dropped.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to spawn the worker thread.
    #[must_use]
    pub fn spawn(cfg: GestureConfig) -> GestureLink {
        let (joint_tx, joint_rx) = sync_channel::<HandSnapshot>(cfg.joint_capacity);
        let (event_tx, event_rx) = sync_channel::<GestureEvent>(cfg.event_capacity);

        let handle = std::thread::Builder::new()
            .name("idtx-gesture".to_owned())
            .spawn(move || worker_loop(&cfg, &joint_rx, &event_tx))
            .expect("spawn gesture worker thread");

        GestureLink {
            joint_tx,
            event_rx,
            handle: Some(handle),
        }
    }
}

fn worker_loop(
    cfg: &GestureConfig,
    joint_rx: &Receiver<HandSnapshot>,
    event_tx: &SyncSender<GestureEvent>,
) {
    let mut left = HandState::new(Hand::Left, cfg);
    let mut right = HandState::new(Hand::Right, cfg);
    let cooldown = cfg.temporal.discrete_cooldown;
    let mut events: Vec<GestureEvent> = Vec::new();

    while let Ok(snap) = joint_rx.recv() {
        #[cfg(feature = "gesture-metrics")]
        let started = std::time::Instant::now();

        events.clear();
        left.update(snap.left.as_ref(), cooldown, &mut events);
        right.update(snap.right.as_ref(), cooldown, &mut events);

        for &ev in &events {
            match ev.phase {
                // Lifecycle edges flip the consumer's active-source set: dropping
                // a `Started` never presses the action, dropping an `Ended` leaves
                // it pressed forever. Block until delivered or the link drops
                // its receiver during shutdown, which cancels pending delivery.
                GesturePhase::Started | GesturePhase::Ended => {
                    if event_tx.send(ev).is_err() {
                        return; // consumer gone
                    }
                }
                // `Held` only refreshes confidence; if the consumer is behind, drop
                // it rather than stall the worker.
                GesturePhase::Held => match event_tx.try_send(ev) {
                    Ok(()) | Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Disconnected(_)) => return,
                },
            }
        }

        #[cfg(feature = "gesture-metrics")]
        eprintln!(
            "[gesture-metrics] frame={} events={} process={:?}",
            snap.frame_index,
            events.len(),
            started.elapsed(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hand::HandPose;
    use glam::Vec3;

    /// A pose with every joint at the origin; individual joints are moved by
    /// the tests to synthesise a target gesture.
    fn base_pose(ts: f64) -> HandPose {
        let mut p = HandPose::zeroed();
        p.timestamp = ts;
        p
    }

    #[test]
    fn shutdown_cancels_edges_when_the_consumer_stops_draining() {
        for event_capacity in [0, 1] {
            let link = GestureWorker::spawn(GestureConfig {
                event_capacity,
                ..GestureConfig::default()
            });
            // Each zeroed hand produces pinch and fist Started edges. Receive
            // only the first: the worker still owes more edges than fit.
            assert!(link.send(HandSnapshot {
                left: Some(base_pose(0.0)),
                right: Some(base_pose(0.0)),
                frame_index: 0,
            }));
            assert_eq!(link.event_rx.recv_timeout(std::time::Duration::from_secs(2))
                .unwrap().phase, GesturePhase::Started);
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let teardown = std::thread::spawn(move || {
                drop(link);
                done_tx.send(()).unwrap();
            });
            done_rx.recv_timeout(std::time::Duration::from_secs(2))
                .expect("gesture teardown must wake a blocked edge sender");
            teardown.join().unwrap();
        }
    }

    fn set(pose: &mut HandPose, joint: usize, pos: Vec3) {
        pose.joints[joint].position = pos;
    }

    #[test]
    fn detects_pinch() {
        let mut pose = base_pose(0.0);
        set(&mut pose, THUMB_TIP, Vec3::new(0.10, 0.0, 0.0));
        set(&mut pose, INDEX_TIP, Vec3::new(0.11, 0.0, 0.0)); // 1 cm apart
        // Push the other fingers out so they don't also read as a fist.
        set(&mut pose, MIDDLE_TIP, Vec3::new(0.0, 0.10, 0.0));
        set(&mut pose, RING_TIP, Vec3::new(0.0, 0.10, 0.0));
        set(&mut pose, LITTLE_TIP, Vec3::new(0.0, 0.10, 0.0));

        let mut out = Vec::new();
        SimpleGestureDetector::default().detect(&pose, &mut out);
        assert!(out.iter().any(|&(k, _)| k == GestureKind::Pinch));
    }

    #[test]
    fn detects_fist() {
        // All four fingertips within curl_distance of the palm (origin).
        let mut pose = base_pose(0.0);
        for t in FINGER_TIPS {
            set(&mut pose, t, Vec3::new(0.0, 0.02, 0.0));
        }
        // Thumb tip near palm too so it isn't read as thumbs-up.
        set(&mut pose, THUMB_TIP, Vec3::new(0.02, 0.0, 0.0));

        let mut out = Vec::new();
        SimpleGestureDetector::default().detect(&pose, &mut out);
        assert!(out.iter().any(|&(k, _)| k == GestureKind::Fist));
        assert!(!out.iter().any(|&(k, _)| k == GestureKind::OpenHand));
    }

    #[test]
    fn detects_open_hand() {
        let mut pose = base_pose(0.0);
        for t in FINGER_TIPS {
            set(&mut pose, t, Vec3::new(0.0, 0.18, 0.0)); // well beyond extend_distance
        }
        set(&mut pose, THUMB_TIP, Vec3::new(0.18, 0.0, 0.0));

        let mut out = Vec::new();
        SimpleGestureDetector::default().detect(&pose, &mut out);
        assert!(out.iter().any(|&(k, _)| k == GestureKind::OpenHand));
        assert!(!out.iter().any(|&(k, _)| k == GestureKind::Fist));
    }

    #[test]
    fn detects_point() {
        let mut pose = base_pose(0.0);
        set(&mut pose, INDEX_TIP, Vec3::new(0.0, 0.18, 0.0)); // extended
        set(&mut pose, MIDDLE_TIP, Vec3::new(0.0, 0.02, 0.0)); // curled
        set(&mut pose, RING_TIP, Vec3::new(0.0, 0.02, 0.0));
        set(&mut pose, LITTLE_TIP, Vec3::new(0.0, 0.02, 0.0));
        set(&mut pose, THUMB_TIP, Vec3::new(0.02, 0.0, 0.0));

        let mut out = Vec::new();
        SimpleGestureDetector::default().detect(&pose, &mut out);
        assert!(out.iter().any(|&(k, _)| k == GestureKind::Point));
    }

    #[test]
    fn point_diagnostics_agrees_with_detector() {
        let cfg = SimpleGestureConfig::default();
        // A clean point pose: index extended, others curled, no pinch.
        let mut pose = base_pose(0.0);
        set(&mut pose, INDEX_TIP, Vec3::new(0.0, 0.18, 0.0));
        set(&mut pose, MIDDLE_TIP, Vec3::new(0.0, 0.02, 0.0));
        set(&mut pose, RING_TIP, Vec3::new(0.0, 0.02, 0.0));
        set(&mut pose, LITTLE_TIP, Vec3::new(0.0, 0.02, 0.0));
        set(&mut pose, THUMB_TIP, Vec3::new(0.05, 0.0, 0.0));
        let d = point_diagnostics(&pose, &cfg);
        assert!(d.is_point(), "{d}");
        assert!(d.index_extended() && d.middle_curled() && !d.pinching());

        // A near-miss: index not extended far enough — the diagnostic pinpoints
        // the single failing condition.
        set(&mut pose, INDEX_TIP, Vec3::new(0.0, 0.10, 0.0));
        let d = point_diagnostics(&pose, &cfg);
        assert!(!d.is_point());
        assert!(!d.index_extended(), "index 0.10 < extend 0.13 → not extended");
        assert!(d.middle_curled(), "other fingers still curled");

        // Both agree with the real detector for the clean pose.
        set(&mut pose, INDEX_TIP, Vec3::new(0.0, 0.18, 0.0));
        let mut out = Vec::new();
        SimpleGestureDetector::new(cfg).detect(&pose, &mut out);
        assert_eq!(
            out.iter().any(|&(k, _)| k == GestureKind::Point),
            point_diagnostics(&pose, &cfg).is_point()
        );
    }

    #[test]
    fn thumb_index_cos_cocked_vs_alongside() {
        // Index points +Y (tip above its proximal).
        let mut pose = base_pose(0.0);
        set(&mut pose, INDEX_PROXIMAL, Vec3::new(0.0, 0.05, 0.0));
        set(&mut pose, INDEX_TIP, Vec3::new(0.0, 0.15, 0.0));
        set(&mut pose, THUMB_METACARPAL, Vec3::new(0.02, 0.03, 0.0));
        // Thumb cocked out (perpendicular to the index → along +X): low cosine.
        set(&mut pose, THUMB_TIP, Vec3::new(0.08, 0.03, 0.0));
        assert!(
            thumb_index_cos(&pose).unwrap() < 0.5,
            "cocked thumb → low cosine"
        );
        // Thumb snapped down alongside the index (also +Y): high cosine.
        set(&mut pose, THUMB_TIP, Vec3::new(0.02, 0.12, 0.0));
        assert!(
            thumb_index_cos(&pose).unwrap() > 0.8,
            "alongside thumb → high cosine"
        );
    }

    #[test]
    fn thumb_index_gap_far_vs_near() {
        let mut pose = base_pose(0.0);
        set(&mut pose, INDEX_PROXIMAL, Vec3::new(0.0, 0.05, 0.0));
        // Thumb raised away from the knuckle → large gap.
        set(&mut pose, THUMB_TIP, Vec3::new(0.06, 0.14, 0.0));
        assert!(thumb_index_gap(&pose) > 0.08, "raised thumb → large gap");
        // Thumb snapped onto the knuckle → small gap.
        set(&mut pose, THUMB_TIP, Vec3::new(0.0, 0.07, 0.01));
        assert!(thumb_index_gap(&pose) < 0.04, "snapped thumb → small gap");
    }

    #[test]
    fn detects_swipe_right() {
        let cfg = TemporalGestureConfig::default();
        let mut det = TemporalGestureDetector::new(cfg);
        // Wrist slides +X 0.20 m over 0.30 s in 10 steps.
        for i in 0..=10u8 {
            let f = f32::from(i);
            let t = f64::from(i) * 0.03;
            let mut pose = base_pose(t);
            set(&mut pose, WRIST, Vec3::new(f * 0.02, 0.0, 0.0));
            det.push(&pose);
        }
        let hits = det.detect(0.30);
        assert!(hits.iter().any(|&(k, _)| k == GestureKind::SwipeRight));
        assert!(!hits.iter().any(|&(k, _)| k == GestureKind::SwipeLeft));
    }

    #[test]
    fn no_swipe_when_still() {
        let mut det = TemporalGestureDetector::new(TemporalGestureConfig::default());
        for i in 0..=10u8 {
            let mut pose = base_pose(f64::from(i) * 0.03);
            set(&mut pose, WRIST, Vec3::new(0.001, 0.0, 0.0));
            det.push(&pose);
        }
        assert!(det.detect(0.30).is_empty());
    }

    #[test]
    fn right_pinch_edges_started_held_ended() {
        // Drive one hand's state machine directly for a deterministic edge
        // sequence (the worker thread path is covered separately).
        let cfg = GestureConfig::default();
        let mut hs = HandState::new(Hand::Right, &cfg);
        let cooldown = cfg.temporal.discrete_cooldown;

        let mut pinch = base_pose(0.0);
        set(&mut pinch, THUMB_TIP, Vec3::new(0.10, 0.0, 0.0));
        set(&mut pinch, INDEX_TIP, Vec3::new(0.11, 0.0, 0.0)); // 1 cm apart
        // Other fingers extended so the pose is a clean pinch (no fist).
        set(&mut pinch, MIDDLE_TIP, Vec3::new(0.0, 0.10, 0.0));
        set(&mut pinch, RING_TIP, Vec3::new(0.0, 0.10, 0.0));
        set(&mut pinch, LITTLE_TIP, Vec3::new(0.0, 0.10, 0.0));

        let find = |out: &[GestureEvent]| {
            out.iter()
                .find(|e| e.kind == GestureKind::Pinch)
                .copied()
                .expect("a pinch event this frame")
        };

        // Frame 1: pinch becomes active → Started.
        let mut out = Vec::new();
        hs.update(Some(&pinch), cooldown, &mut out);
        let ev = find(&out);
        assert_eq!(ev.hand, Hand::Right);
        assert_eq!(ev.phase, GesturePhase::Started);
        assert_eq!(ev.source_string(), "Gesture/pinch/right");

        // Frame 2: still pinching → Held.
        out.clear();
        pinch.timestamp = 0.016;
        hs.update(Some(&pinch), cooldown, &mut out);
        assert_eq!(find(&out).phase, GesturePhase::Held);

        // Frame 3: hand lost → Ended.
        out.clear();
        hs.update(None, cooldown, &mut out);
        assert_eq!(find(&out).phase, GesturePhase::Ended);
    }

    #[test]
    fn discrete_swipe_releases_on_the_next_frame() {
        // A discrete gesture emits only a Started; without a matching Ended the
        // consumer's active-source set would keep it pressed forever. Drive one
        // hand's state machine through a swipe and assert the pulse releases.
        let cfg = GestureConfig::default();
        let mut hs = HandState::new(Hand::Right, &cfg);
        let cooldown = cfg.temporal.discrete_cooldown;

        let swipe_phase = |out: &[GestureEvent]| {
            out.iter()
                .find(|e| e.kind == GestureKind::SwipeRight)
                .map(|e| e.phase)
        };

        // Slide the wrist +X until the swipe fires its Started edge.
        let mut fired = None;
        for i in 0..=12u8 {
            let mut out = Vec::new();
            let mut pose = base_pose(f64::from(i) * 0.03);
            set(&mut pose, WRIST, Vec3::new(f32::from(i) * 0.02, 0.0, 0.0));
            hs.update(Some(&pose), cooldown, &mut out);
            if swipe_phase(&out) == Some(GesturePhase::Started) {
                fired = Some(i);
                break;
            }
        }
        let fired = fired.expect("swipe-right should fire a Started edge");

        // Next frame: wrist held still (well within the cooldown, so no re-fire).
        // The pulse must release with an Ended and no fresh Started.
        let mut out = Vec::new();
        let mut pose = base_pose(f64::from(fired + 1) * 0.03);
        set(&mut pose, WRIST, Vec3::new(f32::from(fired) * 0.02, 0.0, 0.0));
        hs.update(Some(&pose), cooldown, &mut out);
        assert_eq!(
            swipe_phase(&out),
            Some(GesturePhase::Ended),
            "discrete pulse must release next frame: {out:?}"
        );
    }

    #[test]
    fn worker_emits_pinch_over_channel() {
        let link = GestureWorker::spawn(GestureConfig::default());

        let mut pose = base_pose(0.0);
        set(&mut pose, THUMB_TIP, Vec3::new(0.10, 0.0, 0.0));
        set(&mut pose, INDEX_TIP, Vec3::new(0.11, 0.0, 0.0));
        set(&mut pose, MIDDLE_TIP, Vec3::new(0.0, 0.10, 0.0));
        set(&mut pose, RING_TIP, Vec3::new(0.0, 0.10, 0.0));
        set(&mut pose, LITTLE_TIP, Vec3::new(0.0, 0.10, 0.0));

        assert!(link.send(HandSnapshot {
            left: Some(pose),
            right: None,
            frame_index: 0,
        }));

        // Poll briefly for the async result.
        let mut got = None;
        for _ in 0..200 {
            if let Some(ev) = link.try_recv() {
                got = Some(ev);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let ev = got.expect("pinch event from worker");
        assert_eq!(ev.kind, GestureKind::Pinch);
        assert_eq!(ev.hand, Hand::Left);
        assert_eq!(ev.phase, GesturePhase::Started);
        assert_eq!(ev.source_string(), "Gesture/pinch/left");
    }
}
