//! Frozen wire types for the IDTX WASM behavior boundary.
//!
//! This crate is the single source of truth for every value that crosses
//! the host ↔ guest seam. It is `#![no_std]` so it links identically into
//! the wasmtime host (`idtx-behavior`), the guest SDK
//! (`idtx-behavior-sdk`, compiled to `wasm32-unknown-unknown`) and the
//! proc-macro / build tooling.
//!
//! # Stability contract
//!
//! Every type here is `#[repr(C)]` POD (`bytemuck::Pod`) with a
//! compile-time `size_of`/`align_of` guard, and every module exports the
//! same [`ABI_VERSION`]. A guest module re-exports [`ABI_VERSION`] as
//! `_idtx_abi_version`; the host refuses to instantiate a module whose
//! reported version disagrees. **Changing any layout below is a breaking
//! ABI bump** — increment [`ABI_VERSION`] and rebuild every behavior.
//!
//! # glam pinning
//!
//! The POD math types ([`Vec3`], [`Quat`], [`Mat4`], [`Xform`]) convert
//! `From`/`Into` the workspace `glam` types. `glam` is pinned to an
//! **exact** version at the workspace level (`glam = "=0.33.12"`) so the
//! host and every guest observe the identical field layout — the
//! conversions are then pure field moves and the wire form is
//! bit-compatible. The invariant is *host == guest on an identical
//! version*, not any one specific version forever: the pin tracks the
//! `0.33` line (`repr(C)` layout is stable across its patch releases) and
//! was bumped from `0.33.0` to `0.33.12` so the runtime physics engine
//! (rapier/parry via glamx 0.3, which needs glam ^0.33.7) can share these
//! same glam types conversion-free. The layout guards in the test module
//! fail to build if the representation ever drifts.

#![no_std]

use bytemuck::{Pod, Zeroable};

/// ABI revision. Bump on **any** layout change below. A guest module
/// exports this as `_idtx_abi_version`; the host validates parity before
/// instantiation.
///
/// Version `3` covers the widget-interaction seam: the `RAYCAST_BLOCK_WIDGETS`
/// cast flag, the `RAYHIT_WIDGET` result flag (carried in the `RayHitResult`
/// `flags` slot), and the `widget` capability bucket's imports
/// (`idtx_widget_handle` / `idtx_widget_register_handler` /
/// `idtx_widget_event_*`). `RayHitResult` is 32 bytes.
pub const ABI_VERSION: u32 = 3;

// ---------------------------------------------------------------------------
// Entity handle
// ---------------------------------------------------------------------------

/// Opaque entity handle crossing the boundary.
///
/// Encodes `bevy_ecs::Entity::to_bits()` — `hi32 = generation`,
/// `lo32 = index`. [`Entity::NULL`] (all-zero) denotes "no entity" and is
/// what a behavior attached to the stage pseudo-root receives as its
/// `ctx.entity()`.
///
/// The host validates the generation against the live world on every
/// handle-consuming import, so a stale handle returns
/// [`ErrorCode::StaleHandle`] instead of aliasing a recycled slot.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Pod, Zeroable)]
pub struct Entity(pub u64);

impl Entity {
    /// The null handle — the stage pseudo-root, or "no entity".
    pub const NULL: Self = Self(0);

    /// `true` when this is the null handle.
    #[must_use]
    pub const fn is_null(self) -> bool {
        self.0 == 0
    }

    /// Raw `to_bits` value.
    #[must_use]
    pub const fn to_bits(self) -> u64 {
        self.0
    }

    /// Wrap a raw `to_bits` value.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }
}

impl core::fmt::Debug for Entity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_null() {
            write!(f, "Entity(NULL)")
        } else {
            // Mask/shift are exact, so `try_from` never fails; the
            // fallible form keeps us clear of the workspace's forbidden
            // truncating-cast lint.
            let index = u32::try_from(self.0 & 0xFFFF_FFFF).unwrap_or(u32::MAX);
            let generation = u32::try_from(self.0 >> 32).unwrap_or(u32::MAX);
            write!(f, "Entity(index={index}, gen={generation})")
        }
    }
}

// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

/// Result code returned by every fallible host import.
///
/// `#[repr(u32)]` so it crosses the boundary as a plain integer.
/// [`ErrorCode::Ok`] is `0`; every other variant is a distinct failure.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorCode {
    /// Success.
    Ok = 0,
    /// The entity handle referenced a recycled / dead slot.
    StaleHandle = 1,
    /// No entity / attribute matched the request.
    NotFound = 2,
    /// The value existed but had the wrong type.
    TypeMismatch = 3,
    /// The behavior's manifest did not grant this capability.
    CapabilityDenied = 4,
    /// The target is read-only in the current mode.
    ReadOnly = 5,
    /// An argument was malformed (bad pointer, out-of-range, …).
    InvalidArgument = 6,
}

impl ErrorCode {
    /// The raw `u32` wire value.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Reconstruct from a wire `u32`. Unknown values map to
    /// [`ErrorCode::InvalidArgument`] rather than panicking, so a
    /// forward-incompatible guest never traps the host.
    #[must_use]
    pub const fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::Ok,
            1 => Self::StaleHandle,
            2 => Self::NotFound,
            3 => Self::TypeMismatch,
            4 => Self::CapabilityDenied,
            5 => Self::ReadOnly,
            _ => Self::InvalidArgument,
        }
    }

    /// `true` when this is [`ErrorCode::Ok`].
    #[must_use]
    pub const fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }
}

// ---------------------------------------------------------------------------
// POD math wire types
// ---------------------------------------------------------------------------

/// 3-component vector, POD wire form of [`glam::Vec3`].
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Pod, Zeroable, Default)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl From<glam::Vec3> for Vec3 {
    fn from(v: glam::Vec3) -> Self {
        Self {
            x: v.x,
            y: v.y,
            z: v.z,
        }
    }
}

impl From<Vec3> for glam::Vec3 {
    fn from(v: Vec3) -> Self {
        Self::new(v.x, v.y, v.z)
    }
}

/// Quaternion (`x, y, z, w`), POD wire form of [`glam::Quat`].
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Pod, Zeroable)]
pub struct Quat {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Default for Quat {
    fn default() -> Self {
        // Identity quaternion.
        Self {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            w: 1.0,
        }
    }
}

impl From<glam::Quat> for Quat {
    fn from(q: glam::Quat) -> Self {
        Self {
            x: q.x,
            y: q.y,
            z: q.z,
            w: q.w,
        }
    }
}

impl From<Quat> for glam::Quat {
    fn from(q: Quat) -> Self {
        Self::from_xyzw(q.x, q.y, q.z, q.w)
    }
}

/// Column-major 4×4 matrix, POD wire form of [`glam::Mat4`].
///
/// Stored as 16 contiguous `f32`s in column-major order — the same
/// layout `glam::Mat4` uses internally, so the conversions are a
/// `to_cols_array` / `from_cols_array` round-trip.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Pod, Zeroable)]
pub struct Mat4 {
    /// Column-major elements.
    pub cols: [f32; 16],
}

impl Default for Mat4 {
    fn default() -> Self {
        Self::from(glam::Mat4::IDENTITY)
    }
}

impl From<glam::Mat4> for Mat4 {
    fn from(m: glam::Mat4) -> Self {
        Self {
            cols: m.to_cols_array(),
        }
    }
}

impl From<Mat4> for glam::Mat4 {
    fn from(m: Mat4) -> Self {
        Self::from_cols_array(&m.cols)
    }
}

/// A composed transform snapshot (translate / rotate / scale).
///
/// This is the primary transform value crossing the boundary — it mirrors
/// the ECS bridge's decomposed transform form. Padded to a 48-byte layout
/// (`Vec3` translation + `Quat` rotation + `Vec3` scale + 4 bytes tail
/// padding) so the struct's size / alignment are stable and the
/// compile-time guard can assert them.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Pod, Zeroable)]
#[allow(clippy::pub_underscore_fields)] // `_pad` must be `pub` for `Pod`
pub struct Xform {
    /// Local translation.
    pub translation: Vec3,
    /// Local rotation.
    pub rotation: Quat,
    /// Local scale.
    pub scale: Vec3,
    /// Tail padding to a round 48 bytes (`12 + 16 + 12 + 8`). Always zero.
    pub _pad: [u32; 2],
}

impl Default for Xform {
    fn default() -> Self {
        Self {
            translation: Vec3::default(),
            rotation: Quat::default(),
            scale: Vec3 {
                x: 1.0,
                y: 1.0,
                z: 1.0,
            },
            _pad: [0; 2],
        }
    }
}

impl Xform {
    /// Build from decomposed glam parts.
    #[must_use]
    pub fn from_trs(translation: glam::Vec3, rotation: glam::Quat, scale: glam::Vec3) -> Self {
        Self {
            translation: translation.into(),
            rotation: rotation.into(),
            scale: scale.into(),
            _pad: [0; 2],
        }
    }

    /// Compose into a `glam::Mat4`.
    #[must_use]
    pub fn to_matrix(self) -> glam::Mat4 {
        glam::Mat4::from_scale_rotation_translation(
            self.scale.into(),
            self.rotation.into(),
            self.translation.into(),
        )
    }
}

// ---------------------------------------------------------------------------
// Input wire enums (§7)
// ---------------------------------------------------------------------------

/// Resolved identifier for a named input action.
///
/// The SDK resolves an action *name* to this id once at `on_attach` via
/// `idtx_input_resolve`, so hot-path reads are integer lookups. `0` is a
/// reserved "unresolved / unknown action" sentinel.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Pod, Zeroable, Default)]
pub struct InputActionId(pub u32);

impl InputActionId {
    /// The unresolved / unknown-action sentinel.
    pub const UNRESOLVED: Self = Self(0);

    /// `true` when the action failed to resolve.
    #[must_use]
    pub const fn is_unresolved(self) -> bool {
        self.0 == 0
    }
}

/// Phase of a button-style input action for the current frame.
///
/// `#[repr(u32)]` so it crosses as a plain integer. [`InputPhase::None`]
/// (`0`) means the action is inactive this frame.
///
/// A guest may opt into edge-driven input delivery by exporting
/// `_idtx_on_input(u32 action_id, u32 phase)`: the host calls it once
/// per changed button action per frame while *playing*, passing the
/// [`InputActionId`] and this phase's [`InputPhase::as_u32`] code. Missing
/// export ⇒ silent no-op. Reusing the existing [`InputActionId`] +
/// [`InputPhase`] wire types keeps the hook purely additive — it does **not**
/// bump [`ABI_VERSION`].
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum InputPhase {
    /// Not active this frame.
    #[default]
    None = 0,
    /// Transitioned to active this frame.
    Pressed = 1,
    /// Held active (was active last frame, still active).
    Held = 2,
    /// Transitioned to inactive this frame.
    Released = 3,
}

impl InputPhase {
    /// The raw `u32` wire value.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Reconstruct from a wire `u32`. Unknown values map to
    /// [`InputPhase::None`].
    #[must_use]
    pub const fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Pressed,
            2 => Self::Held,
            3 => Self::Released,
            _ => Self::None,
        }
    }
}

// ---------------------------------------------------------------------------
// Behavior kind
// ---------------------------------------------------------------------------

/// Execution profile of a behavior.
///
/// Distinguishes gameplay behaviors (run while *playing*) from tooling /
/// constraint behaviors (run in the conductor's *authoring* mode).
/// `#[repr(u32)]` so it crosses as a plain integer and is stored in the
/// guest descriptor (§3.1).
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BehaviorKind {
    /// Gameplay behavior — full playing lifecycle.
    #[default]
    Runtime = 0,
    /// Tooling / constraint behavior — `on_editor_update` only, in the
    /// conductor's authoring mode.
    Editor = 1,
}

impl BehaviorKind {
    /// The raw `u32` wire value.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Reconstruct from a wire `u32`. Unknown values map to
    /// [`BehaviorKind::Runtime`].
    #[must_use]
    pub const fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Editor,
            _ => Self::Runtime,
        }
    }

    /// The token this variant round-trips to in the manifest / customData.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Editor => "editor",
        }
    }

    /// Parse a manifest token. Unknown tokens map to
    /// [`BehaviorKind::Runtime`].
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "editor" => Self::Editor,
            _ => Self::Runtime,
        }
    }
}

// ---------------------------------------------------------------------------
// Stage-level lifecycle events
// ---------------------------------------------------------------------------

/// A stage- / level-scoped lifecycle event delivered to behaviors attached
/// to the stage pseudo-root (those whose `ctx.entity()` is [`Entity::NULL`]).
///
/// Where the per-entity hooks (`on_start` / `on_stop`) track a single
/// behavior's transport participation, these events broadcast the *stage's*
/// coarse lifecycle: the level being brought up or torn down, and the
/// play transport starting, stopping, pausing and resuming. A guest opts in
/// by exporting `_idtx_on_stage_event(u32)`; the host passes the raw
/// [`StageEvent::as_u32`] code. Missing export ⇒ silent no-op.
///
/// `#[repr(u32)]` so it crosses the boundary as a plain integer. Adding this
/// import/export is purely additive — it does **not** bump [`ABI_VERSION`].
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StageEvent {
    /// The stage/level has been loaded and hydrated; behaviors are attached
    /// but the transport has not necessarily started.
    #[default]
    Loaded = 0,
    /// The stage/level is about to be torn down; last chance to react
    /// before instances are destroyed.
    Unloading = 1,
    /// The play transport transitioned into `Playing`.
    PlayStart = 2,
    /// The play transport transitioned out of `Playing` (back to authoring).
    PlayStop = 3,
    /// The running transport was paused (still `Playing`, ticks suspended).
    Paused = 4,
    /// A paused transport resumed ticking.
    Resumed = 5,
}

impl StageEvent {
    /// The raw `u32` wire value.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Reconstruct from a wire `u32`. Unknown values map to
    /// [`StageEvent::Loaded`].
    #[must_use]
    pub const fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Unloading,
            2 => Self::PlayStart,
            3 => Self::PlayStop,
            4 => Self::Paused,
            5 => Self::Resumed,
            _ => Self::Loaded,
        }
    }

    /// The token this variant round-trips to (diagnostics / logging).
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Loaded => "loaded",
            Self::Unloading => "unloading",
            Self::PlayStart => "play_start",
            Self::PlayStop => "play_stop",
            Self::Paused => "paused",
            Self::Resumed => "resumed",
        }
    }

    /// Parse a token. Unknown tokens map to [`StageEvent::Loaded`].
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "unloading" => Self::Unloading,
            "play_start" => Self::PlayStart,
            "play_stop" => Self::PlayStop,
            "paused" => Self::Paused,
            "resumed" => Self::Resumed,
            _ => Self::Loaded,
        }
    }
}

// ---------------------------------------------------------------------------
// Playback control
// ---------------------------------------------------------------------------

/// A playback-transport command a behavior issues to the host to drive an
/// [`AnimClipPlayer`](../idtx_scene/index.html) playhead.
///
/// Guests with the `playback` capability call `idtx_playback_command(target,
/// clip_ptr, clip_len, kind, value)`; the host defers each call into a
/// per-frame queue and drains it after `run_frame`, applying the command to
/// the addressed entity's clip playhead. The `value` argument is interpreted
/// per variant (see each doc line); it is ignored where not mentioned.
///
/// `#[repr(u32)]` so it crosses the boundary as a plain integer. Adding this
/// import is purely additive — it does **not** bump [`ABI_VERSION`].
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PlaybackCommandKind {
    /// Start (or resume) the addressed playhead. `value` ignored.
    #[default]
    Play = 0,
    /// Pause the playhead in place. `value` ignored.
    Pause = 1,
    /// Stop and rewind the playhead to its start. `value` ignored.
    Stop = 2,
    /// Move the playhead to `value` (a time code), clamped to the clip range.
    Seek = 3,
    /// Set the playback speed multiplier to `value` (may be negative to play
    /// in reverse).
    SetSpeed = 4,
    /// Enable looping when `value != 0.0`, disable it otherwise.
    SetLooping = 5,
}

impl PlaybackCommandKind {
    /// The raw `u32` wire value.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Reconstruct from a wire `u32`. Unknown values map to
    /// [`PlaybackCommandKind::Play`].
    #[must_use]
    pub const fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Pause,
            2 => Self::Stop,
            3 => Self::Seek,
            4 => Self::SetSpeed,
            5 => Self::SetLooping,
            _ => Self::Play,
        }
    }

    /// The token this variant round-trips to (diagnostics / logging).
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Stop => "stop",
            Self::Seek => "seek",
            Self::SetSpeed => "set_speed",
            Self::SetLooping => "set_looping",
        }
    }

    /// Parse a token. Unknown tokens map to [`PlaybackCommandKind::Play`].
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "pause" => Self::Pause,
            "stop" => Self::Stop,
            "seek" => Self::Seek,
            "set_speed" => Self::SetSpeed,
            "set_looping" => Self::SetLooping,
            _ => Self::Play,
        }
    }
}

// ---------------------------------------------------------------------------
// XR pose wire types
// ---------------------------------------------------------------------------

/// Opaque handle to a tracked XR space (HMD, left / right controller grip,
/// left / right controller aim).
///
/// `#[repr(u32)]` so it crosses the boundary as a plain integer. Passed to
/// `idtx_xr_get_pose` to select which tracked space's pose to read.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum XrSpace {
    /// The head-mounted display.
    #[default]
    Hmd = 0,
    /// The left-hand controller **grip** pose (where the hand holds the device).
    LeftHand = 1,
    /// The right-hand controller **grip** pose.
    RightHand = 2,
    /// The left-hand controller **aim** pose (the pointer/ray origin+direction).
    LeftAim = 3,
    /// The right-hand controller **aim** pose.
    RightAim = 4,
}

impl XrSpace {
    /// The raw `u32` wire value.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Reconstruct from a wire `u32`. Unknown values map to
    /// [`XrSpace::Hmd`].
    #[must_use]
    pub const fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::LeftHand,
            2 => Self::RightHand,
            3 => Self::LeftAim,
            4 => Self::RightAim,
            _ => Self::Hmd,
        }
    }
}

/// Pose returned by `idtx_xr_get_pose`.
///
/// Layout: [`Xform`] (48 bytes) + validity flag (`u32`) + pad (`u32`) =
/// 56 bytes total. The scale of the embedded `Xform` is always unit; only
/// translation + rotation carry the tracked pose.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Default, Pod, Zeroable)]
#[allow(clippy::pub_underscore_fields)] // `_pad` must be `pub` for `Pod`
pub struct XrPoseResult {
    /// The tracked pose (translation + rotation; unit scale).
    pub xform: Xform,
    /// `0` = not tracked (pose is undefined), `1` = tracked.
    pub valid: u32,
    /// Tail padding to a round 56 bytes. Always zero.
    pub _pad: u32,
}

impl XrPoseResult {
    /// Build a tracked (valid) result from a pose transform.
    #[must_use]
    pub fn tracked(xform: Xform) -> Self {
        Self {
            xform,
            valid: 1,
            _pad: 0,
        }
    }

    /// The untracked sentinel (identity pose, `valid == 0`).
    #[must_use]
    pub fn untracked() -> Self {
        Self::default()
    }

    /// `true` when the pose is tracked / valid.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.valid != 0
    }
}

/// Metadata for an HTTP response, written by the `idtx_http_request` host import
/// (capability: `net`) to the caller's result pointer.
///
/// Used only by the **synchronous** (blocking) egress import. The asynchronous
/// path (`idtx_http_request_async` + the `_idtx_on_http_response(request_id,
/// status)` guest export + the `idtx_http_response_body` import) passes the
/// status as a hook argument and reads the body on demand, so it needs no wire
/// struct. Both the sync and async import/export group are **additive** — adding
/// them does **not** bump [`ABI_VERSION`].
///
/// The response body itself is written separately into the caller-provided
/// buffer (truncated to its capacity); [`body_len`](Self::body_len) is the
/// response's *true* length so the guest can detect truncation and, if it cares,
/// retry with a larger buffer — the same caller-buffer + true-length convention
/// used by [`RayHitResult`]'s path output.
///
/// Layout: `status` (`u16`) + `_pad` (`u16`) + `body_len` (`u32`) = 8 bytes.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Pod, Zeroable)]
#[allow(clippy::pub_underscore_fields)] // `_pad` must be `pub` for `Pod`
pub struct HttpResult {
    /// The HTTP status code (e.g. `200`). `0` means the request never completed
    /// (a transport-level failure: DNS, connect, TLS, timeout); the host also
    /// returns [`ErrorCode::InvalidArgument`] in that case.
    pub status: u16,
    /// Padding to a 4-byte boundary. Always zero.
    pub _pad: u16,
    /// The response body's *true* length in bytes, which may exceed the caller
    /// buffer's capacity (in which case only the capacity-many leading bytes
    /// were written).
    pub body_len: u32,
}

// ---------------------------------------------------------------------------
// XR hand-tracking wire types
// ---------------------------------------------------------------------------

/// Number of joints in an `OpenXR` hand skeleton (`XR_HAND_JOINT_COUNT_EXT`).
pub const XR_HAND_JOINT_COUNT: usize = 26;

/// Which hand to query via `idtx_xr_get_hand_joints`.
///
/// `#[repr(u32)]` so it crosses as a plain integer. Unknown values are
/// treated as [`XrSide::Left`].
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum XrSide {
    #[default]
    Left = 0,
    Right = 1,
}

impl XrSide {
    /// The raw `u32` wire value.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Reconstruct from a wire `u32`. Unknown values map to [`XrSide::Left`].
    #[must_use]
    pub const fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Right,
            _ => Self::Left,
        }
    }
}

/// Which input source currently drives a hand/controller `side`, as a bitmask
/// returned directly by `idtx_xr_get_tracking_mode` (capability: `xr_hmd`).
///
/// The two bits are independent and may combine: a runtime can report a
/// controller **and** hand tracking for the same side in the same frame, so a
/// behavior that needs the exact source must test the bits rather than assume
/// they are mutually exclusive. `NONE` (`0`) means the side is not tracked by
/// either source this frame.
///
/// `#[repr(transparent)]` over `u32` so it crosses the boundary as the plain
/// integer the import returns. Additive — introducing it does **not** bump
/// [`ABI_VERSION`] (an older guest simply never imports the new symbol).
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Pod, Zeroable)]
pub struct XrInputSource(pub u32);

impl XrInputSource {
    /// Neither a controller nor hand tracking drives this side this frame.
    pub const NONE: Self = Self(0);
    /// A physical controller's grip/aim pose drives this side.
    pub const CONTROLLER: Self = Self(1 << 0);
    /// An articulated hand skeleton (hand tracking) drives this side.
    pub const HAND: Self = Self(1 << 1);

    /// Mask of all bits currently defined. Used to drop unknown (future) bits
    /// so a newer host can add a source without confusing an older reader.
    const KNOWN: u32 = Self::CONTROLLER.0 | Self::HAND.0;

    /// The raw `u32` wire value.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Reconstruct from a wire `u32`, masking off any unknown (forward-compat)
    /// bits so an older guest reading a newer host's value stays well-defined.
    #[must_use]
    pub const fn from_bits(v: u32) -> Self {
        Self(v & Self::KNOWN)
    }

    /// `true` when a physical controller drives this side.
    #[must_use]
    pub const fn has_controller(self) -> bool {
        self.0 & Self::CONTROLLER.0 != 0
    }

    /// `true` when hand tracking drives this side.
    #[must_use]
    pub const fn has_hand(self) -> bool {
        self.0 & Self::HAND.0 != 0
    }

    /// `true` when either source is tracking this side (i.e. not [`NONE`]).
    ///
    /// [`NONE`]: Self::NONE
    #[must_use]
    pub const fn is_tracked(self) -> bool {
        self.0 != 0
    }

    /// Set the controller bit (builder-style), leaving the hand bit untouched.
    #[must_use]
    pub const fn with_controller(self, present: bool) -> Self {
        if present {
            Self(self.0 | Self::CONTROLLER.0)
        } else {
            Self(self.0 & !Self::CONTROLLER.0)
        }
    }

    /// Set the hand bit (builder-style), leaving the controller bit untouched.
    #[must_use]
    pub const fn with_hand(self, present: bool) -> Self {
        if present {
            Self(self.0 | Self::HAND.0)
        } else {
            Self(self.0 & !Self::HAND.0)
        }
    }
}

/// One tracked hand joint — position, orientation, and capsule radius.
///
/// Flattened 26-element arrays of this type are written to the guest by
/// `idtx_xr_get_hand_joints`. Layout: `[f32; 3]` position + `f32` radius +
/// `[f32; 4]` orientation (xyzw) + `u32` valid + `u32` padding = 40 bytes.
///
/// The `valid` field is `1` when the joint's position and orientation flags
/// are set by the `OpenXR` runtime; `0` when the joint is not tracked (the
/// remaining fields are undefined).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[allow(clippy::pub_underscore_fields)]
pub struct XrHandJointResult {
    /// World-space position (metres).
    pub position: [f32; 3],
    /// Joint capsule radius in metres.
    pub radius: f32,
    /// Orientation quaternion (x, y, z, w).
    pub orientation: [f32; 4],
    /// `1` = tracked, `0` = not tracked.
    pub valid: u32,
    /// Padding to 40 bytes. Always zero.
    pub _pad: u32,
}

// ---------------------------------------------------------------------------
// Overlay / render command channel
// ---------------------------------------------------------------------------

/// A render-overlay command a behavior issues to the host to draw debug /
/// gameplay overlay geometry, highlight prims, override materials, or drive a
/// post-process parameter.
///
/// Guests with the `overlay` capability call `idtx_overlay_command(kind,
/// target, a_ptr, b_ptr, color_ptr, scalar)`; the host defers each call into a
/// per-frame queue and drains it after `run_frame`, feeding the overlay render
/// pass and post-process chain. Overlay commands are transient —
/// they describe one frame's overlay and never author into USD (that is the
/// `stage_edit` bucket). The pointer arguments are interpreted per variant (see
/// each doc line) and ignored where not mentioned.
///
/// `#[repr(u32)]` so it crosses the boundary as a plain integer. Adding this
/// import is purely additive — it does **not** bump [`ABI_VERSION`].
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OverlayCommandKind {
    /// Draw a world-space line segment from `a` to `b` in `color`; `scalar` is
    /// the line width in metres.
    #[default]
    DrawLine = 0,
    /// Draw a ballistic arc from `a` along direction `b` at initial speed
    /// `scalar`, in `color` (sampled to a polyline by the builder).
    DrawArc = 1,
    /// Highlight the `target` prim with a coloured wireframe box in `color`
    /// (the low-risk fallback cue; see also [`Self::HighlightTint`] and
    /// [`Self::HighlightOutline`]).
    Highlight = 2,
    /// Clear any highlight on the `target` prim.
    ClearHighlight = 3,
    /// Override the `target` prim's material; the override material is named by
    /// the string marshalled through `a_ptr`.
    SetMaterialOverride = 4,
    /// Clear the `target` prim's material override.
    ClearMaterialOverride = 5,
    /// Set a named post-process parameter (name via `a_ptr`) to `scalar`.
    SetPostprocessParam = 6,
    /// Tint the `target` prim's surface with `color` — linear RGBA where the
    /// alpha channel is the tint strength (0 = untouched, 1 = fully `color`).
    HighlightTint = 7,
    /// Draw a silhouette outline around the `target` prim in `color`; outline
    /// thickness/softness come from the post-process outline params (see
    /// [`Self::SetPostprocessParam`]).
    HighlightOutline = 8,
}

impl OverlayCommandKind {
    /// The raw `u32` wire value.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Reconstruct from a wire `u32`. Unknown values map to
    /// [`OverlayCommandKind::DrawLine`].
    #[must_use]
    pub const fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::DrawArc,
            2 => Self::Highlight,
            3 => Self::ClearHighlight,
            4 => Self::SetMaterialOverride,
            5 => Self::ClearMaterialOverride,
            6 => Self::SetPostprocessParam,
            7 => Self::HighlightTint,
            8 => Self::HighlightOutline,
            _ => Self::DrawLine,
        }
    }

    /// The token this variant round-trips to (diagnostics / logging).
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::DrawLine => "draw_line",
            Self::DrawArc => "draw_arc",
            Self::Highlight => "highlight",
            Self::ClearHighlight => "clear_highlight",
            Self::SetMaterialOverride => "set_material_override",
            Self::ClearMaterialOverride => "clear_material_override",
            Self::SetPostprocessParam => "set_postprocess_param",
            Self::HighlightTint => "highlight_tint",
            Self::HighlightOutline => "highlight_outline",
        }
    }

    /// Parse a token. Unknown tokens map to [`OverlayCommandKind::DrawLine`].
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "draw_arc" => Self::DrawArc,
            "highlight" => Self::Highlight,
            "clear_highlight" => Self::ClearHighlight,
            "set_material_override" => Self::SetMaterialOverride,
            "clear_material_override" => Self::ClearMaterialOverride,
            "set_postprocess_param" => Self::SetPostprocessParam,
            "highlight_tint" => Self::HighlightTint,
            "highlight_outline" => Self::HighlightOutline,
            _ => Self::DrawLine,
        }
    }
}

// ---------------------------------------------------------------------------
// Scene query / raycast wire types (scene interaction)
// ---------------------------------------------------------------------------
//
// All POD, all size-guarded below. Adding these structs + their imports is
// purely additive — it does **not** bump [`ABI_VERSION`].

/// Raycast is restricted to prims flagged with the collision API schema
/// (`idtx:collision:enabled`); non-collidable prims are passed through.
pub const RAYCAST_REQUIRE_COLLIDABLE: u32 = 1 << 0;
/// Invisible prims (via `VisibilityComp`) never collide. Set by default by the
/// SDK; cleared to let a ray hit hidden geometry.
pub const RAYCAST_RESPECT_VISIBILITY: u32 = 1 << 1;
/// Invert the type filter: when set, `type_filter_hash` *excludes* the named
/// type instead of requiring it.
pub const RAYCAST_TYPE_EXCLUDE: u32 = 1 << 2;
/// Let `IdtxWidgetPanel` quads participate in this cast so a widget occludes
/// like any geometry (nearest hit wins). Cleared by default: a cast passes
/// *through* widgets unless it opts in. A widget-nearest result is tagged with
/// [`RAYHIT_WIDGET`]. Only visible, in-world, non-transparent panels are ever
/// tested (see the session backend); an invisible widget is never hit.
pub const RAYCAST_BLOCK_WIDGETS: u32 = 1 << 3;

/// [`RayHitResult::flags`] bit: the hit prim is an `IdtxWidgetPanel` (set only
/// when the cast used [`RAYCAST_BLOCK_WIDGETS`] and a panel was the nearest
/// hit). Lets the guest branch on `RayHit::is_widget()` without a type lookup.
pub const RAYHIT_WIDGET: u32 = 1 << 0;

/// Sentinel `type_filter_hash` meaning "any prim type" (no type filter). The
/// FNV-1a of a non-empty token never collides with this (empty-token FNV is the
/// non-zero offset basis).
pub const TYPE_HASH_ANY: u64 = 0;

/// USD imageable purpose mask bits for `purpose_mask`. `0` is treated as
/// [`PURPOSE_ALL`] by the backend (unfiltered).
pub const PURPOSE_DEFAULT: u32 = 1 << 0;
/// `render` purpose.
pub const PURPOSE_RENDER: u32 = 1 << 1;
/// `proxy` purpose.
pub const PURPOSE_PROXY: u32 = 1 << 2;
/// `guide` purpose.
pub const PURPOSE_GUIDE: u32 = 1 << 3;
/// Every purpose (the unfiltered default).
pub const PURPOSE_ALL: u32 = PURPOSE_DEFAULT | PURPOSE_RENDER | PURPOSE_PROXY | PURPOSE_GUIDE;

/// Collision *query-channel* mask bits for [`RaycastParams::channel_mask`].
///
/// A collider answers a cast when the cast's mask is [`CHANNEL_ANY`] *or* the
/// mask shares a bit with the collider's own channels (`idtx:collision:channel`
/// → these bits; an `All` collider sets both `PHYSICS | INTERACTION`).
///
/// # ABI invariant — `CHANNEL_ANY == 0`
///
/// This field reuses the former tail-padding slot. Behaviour wasm compiled
/// before channels existed zero-fills it, so **`0` must mean "unfiltered"** — a
/// zeroed `channel_mask` casts exactly as it did before this field existed. Any
/// other encoding (e.g. `0` = "match no channel") would silently turn every
/// legacy behaviour raycast into a miss. Pinned by the `zero_channel_mask_*`
/// test and the layout guard below (still 48 bytes, so `ABI_VERSION` is
/// unchanged).
pub const CHANNEL_ANY: u32 = 0;
/// The physics-simulation query domain.
pub const CHANNEL_PHYSICS: u32 = 1 << 0;
/// The interaction (gaze / highlight / grab) query domain.
pub const CHANNEL_INTERACTION: u32 = 1 << 1;

/// 64-bit FNV-1a of a prim-type token (e.g. `"Mesh"`, `"DomeLight"`), computed
/// identically on the guest (`const`) and the host so a type filter never
/// marshals a string across the hot path. Empty input yields the offset basis,
/// which is non-zero and so distinct from [`TYPE_HASH_ANY`].
#[must_use]
pub const fn type_hash(token: &str) -> u64 {
    let bytes = token.as_bytes();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

/// Parameters for `idtx_raycast` (read by the host from guest memory).
///
/// Layout: `Vec3` origin (12) + `Vec3` dir (12) + `u64` type hash (8) +
/// `f32` max distance (4) + `u32` flags (4) + `u32` purpose mask (4) +
/// `u32` channel mask (4) = 48 bytes. `dir` need not be normalized.
/// `max_distance <= 0` means unbounded. The trailing `channel_mask` occupies
/// what was tail padding, so the size is unchanged and old wasm (which zeroed
/// it) casts unfiltered — see [`CHANNEL_ANY`].
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Default, Pod, Zeroable)]
pub struct RaycastParams {
    /// Ray origin in USD/stage world space (metres).
    pub origin: Vec3,
    /// Ray direction in USD/stage world space (need not be unit length).
    pub dir: Vec3,
    /// [`type_hash`] of the required/excluded prim type, or [`TYPE_HASH_ANY`].
    pub type_filter_hash: u64,
    /// Maximum hit distance in metres; `<= 0` means unbounded.
    pub max_distance: f32,
    /// `RAYCAST_*` flag bits.
    pub flags: u32,
    /// `PURPOSE_*` mask; `0` is treated as [`PURPOSE_ALL`].
    pub purpose_mask: u32,
    /// `CHANNEL_*` query-channel mask; [`CHANNEL_ANY`] (`0`) is unfiltered.
    pub channel_mask: u32,
}

/// Result of `idtx_raycast` (written by the host into guest memory).
///
/// Layout: `Entity` (8) + `Vec3` position (12) + `f32` distance (4) +
/// `u32` hit (4) + `u32` flags (4) = 32 bytes. The hit prim's path is written
/// separately into a caller buffer (true length returned by the import). The
/// trailing slot was tail padding before ABI 3; it now carries `RAYHIT_*`
/// bits, so the size is unchanged and old wasm (which zeroed it) reads
/// `flags == 0` = "no extra bits".
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Default, Pod, Zeroable)]
pub struct RayHitResult {
    /// The hit prim's entity handle ([`Entity::NULL`] on miss).
    pub entity: Entity,
    /// World-space hit position (metres); undefined on miss.
    pub position: Vec3,
    /// World-space distance from origin to hit; undefined on miss.
    pub distance: f32,
    /// `1` = hit, `0` = miss.
    pub hit: u32,
    /// `RAYHIT_*` flag bits (e.g. [`RAYHIT_WIDGET`]). Zero when no bits apply.
    pub flags: u32,
}

/// Parameters for `idtx_arc_cast` (read by the host from guest memory).
///
/// Layout: `Vec3` origin (12) + `Vec3` dir (12) + `u64` type hash (8) +
/// `f32` speed (4) + `f32` gravity (4) + `f32` `max_time` (4) +
/// `u32` `sample_count` (4) + `u32` flags (4) + `u32` `purpose_mask` (4) =
/// 56 bytes. The arc is `p(t) = origin + dir̂·speed·t + ½·g·t²` on world −Y,
/// sampled at `sample_count` points up to `max_time` (or the first hit).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Default, Pod, Zeroable)]
pub struct ArcCastParams {
    /// Launch origin in USD/stage world space (metres).
    pub origin: Vec3,
    /// Launch direction (need not be unit length).
    pub dir: Vec3,
    /// [`type_hash`] of the required/excluded prim type, or [`TYPE_HASH_ANY`].
    pub type_filter_hash: u64,
    /// Launch speed (m/s) along `dir`.
    pub speed: f32,
    /// Downward acceleration on world +Y (m/s²); e.g. `9.81`.
    pub gravity: f32,
    /// Maximum flight time to sample (seconds).
    pub max_time: f32,
    /// Number of arc points to return (density); clamped to the caller buffer.
    pub sample_count: u32,
    /// `RAYCAST_*` flag bits (shared with raycast).
    pub flags: u32,
    /// `PURPOSE_*` mask; `0` is treated as [`PURPOSE_ALL`].
    pub purpose_mask: u32,
}

/// Result of `idtx_arc_cast` (written by the host into guest memory).
///
/// Layout: `Entity` (8) + `Vec3` position (12) + `f32` distance (4) +
/// `u32` hit (4) + `u32` `sample_count` (4) = 32 bytes. The arc polyline points
/// are written separately into a caller `Vec3` buffer; `sample_count` is the
/// true number produced (so the guest detects truncation).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Default, Pod, Zeroable)]
pub struct ArcHitResult {
    /// The hit prim's entity handle ([`Entity::NULL`] when the arc hit nothing).
    pub hit_entity: Entity,
    /// World-space hit position (metres); undefined when `hit == 0`.
    pub hit_position: Vec3,
    /// World-space arc length from origin to the hit; undefined when `hit == 0`.
    pub hit_distance: f32,
    /// `1` = the arc struck a prim, `0` = no hit within `max_time`.
    pub hit: u32,
    /// True number of arc points produced (may exceed the caller buffer).
    pub sample_count: u32,
}

/// The active view (player) camera's world pose, written by `idtx_camera_pose`.
///
/// Lets a behavior cast a ray from the viewpoint it is actually seeing — the
/// desktop orbit/player camera, or (in XR) the HMD. All vectors are in **stage
/// space** (the frame rays/arcs are expressed in). `forward` is the normalized
/// look direction; `up` the camera up. Layout: `Vec3` position (12) +
/// `Vec3` forward (12) + `Vec3` up (12) + `u32` valid (4) = 40 bytes.
///
/// Reading this import is purely additive — it does **not** bump
/// [`ABI_VERSION`].
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Default, Pod, Zeroable)]
pub struct CameraPose {
    /// Camera world-space position (stage units).
    pub position: Vec3,
    /// Normalized world-space look direction.
    pub forward: Vec3,
    /// World-space camera up vector.
    pub up: Vec3,
    /// `1` = a camera pose is available this frame, `0` = none (fields zero).
    pub valid: u32,
}

// ---------------------------------------------------------------------------
// Widget-interaction wire constants (ABI 3 — `widget` capability bucket)
// ---------------------------------------------------------------------------
//
// The host delivers a widget event to the panel's behavior through the existing
// `_idtx_on_event` seam with a reserved name `idtx:widget:<kind>`; the guest
// reads the element path / UV / kind through the `idtx_widget_event_*` imports.
// These are POD scalars and strings, so no new struct crosses the boundary.

/// Event-name prefix for a host-dispatched widget event. The macro-generated
/// `_idtx_on_event` dispatcher routes a current event whose name starts with
/// this to the per-behavior widget-handler registry; any other event falls
/// through to the author's `on_event`.
pub const WIDGET_EVENT_PREFIX: &str = "idtx:widget:";

/// Sentinel returned by `idtx_widget_event_kind` when the event currently being
/// delivered to `_idtx_on_event` is **not** a widget event (an ordinary posted
/// event, or none). The always-linked macro dispatcher uses it to decide
/// whether to route to the widget-handler registry or the author's `on_event`.
pub const WIDGET_EVENT_NONE: u32 = u32::MAX;

/// A widget interaction kind. Carried in `idtx_widget_register_handler`'s
/// `kind` argument, `idtx_widget_event_kind`'s return, and the
/// `idtx:widget:<kind>` event name. [`Self::Click`] is implemented;
/// `Scroll`/`Secondary` are reserved for the same `widgets.*` action group.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum WidgetEventKind {
    /// Primary activation (`widgets.click`); the only implemented kind.
    #[default]
    Click = 0,
    /// Reserved: scroll / wheel (`widgets.scroll`).
    Scroll = 1,
    /// Reserved: secondary / context action (`widgets.secondary`).
    Secondary = 2,
}

impl WidgetEventKind {
    /// Decode a wire `u32`; unknown values fall back to [`Self::Click`].
    #[must_use]
    pub const fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Scroll,
            2 => Self::Secondary,
            _ => Self::Click,
        }
    }

    /// The wire `u32` encoding.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// The `idtx:widget:<kind>` event-name suffix token (`"click"`, …).
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Click => "click",
            Self::Scroll => "scroll",
            Self::Secondary => "secondary",
        }
    }

    /// The baseline `widgets.<kind>` input-action name the host resolves to
    /// decide whether this kind fired (`"widgets.click"`, …).
    #[must_use]
    pub const fn action_name(self) -> &'static str {
        match self {
            Self::Click => "widgets.click",
            Self::Scroll => "widgets.scroll",
            Self::Secondary => "widgets.secondary",
        }
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

/// Compile-time assertions locking the wire layout. Any drift in glam's
/// internal representation (or an accidental field edit) turns these into
/// a build error rather than silent memory corruption across the seam.
const _: () = {
    assert!(core::mem::size_of::<Entity>() == 8);
    assert!(core::mem::size_of::<Vec3>() == 12);
    assert!(core::mem::size_of::<Quat>() == 16);
    assert!(core::mem::size_of::<Mat4>() == 64);
    assert!(core::mem::size_of::<Xform>() == 48);
    assert!(core::mem::size_of::<InputActionId>() == 4);
    assert!(core::mem::size_of::<XrPoseResult>() == 56);
    // Full hand: 26 × 40 = 1 040 bytes.
    assert!(core::mem::size_of::<XrHandJointResult>() == 40);
    // Scene-interaction wire types.
    assert!(core::mem::size_of::<RaycastParams>() == 48);
    assert!(core::mem::size_of::<RayHitResult>() == 32);
    assert!(core::mem::size_of::<ArcCastParams>() == 56);
    assert!(core::mem::size_of::<ArcHitResult>() == 32);
    assert!(core::mem::size_of::<CameraPose>() == 40);

    // glam parity: our wire types must match glam's own sizes so the
    // `From`/`Into` conversions are pure field moves.
    assert!(core::mem::size_of::<Vec3>() == core::mem::size_of::<glam::Vec3>());
    assert!(core::mem::size_of::<Quat>() == core::mem::size_of::<glam::Quat>());
    assert!(core::mem::size_of::<Mat4>() == core::mem::size_of::<glam::Mat4>());
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_types_are_pod() {
        let v = Vec3 {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        };
        let bytes = bytemuck::bytes_of(&v);
        let back: Vec3 = *bytemuck::from_bytes(bytes);
        assert_eq!(v, back);

        let x = Xform::default();
        let bytes = bytemuck::bytes_of(&x);
        let back: Xform = *bytemuck::from_bytes(bytes);
        assert_eq!(x, back);
    }

    #[test]
    fn scene_query_wire_types_round_trip() {
        let rp = RaycastParams {
            origin: Vec3 {
                x: 1.0,
                y: 2.0,
                z: 3.0,
            },
            dir: Vec3 {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
            type_filter_hash: type_hash("Mesh"),
            max_distance: 100.0,
            flags: RAYCAST_REQUIRE_COLLIDABLE | RAYCAST_RESPECT_VISIBILITY,
            purpose_mask: PURPOSE_ALL,
            channel_mask: CHANNEL_INTERACTION,
        };
        assert_eq!(
            *bytemuck::from_bytes::<RaycastParams>(bytemuck::bytes_of(&rp)),
            rp
        );

        let rh = RayHitResult {
            entity: Entity::from_bits(42),
            position: Vec3 {
                x: 1.0,
                y: 0.0,
                z: 5.0,
            },
            distance: 5.0,
            hit: 1,
            flags: RAYHIT_WIDGET,
        };
        assert_eq!(
            *bytemuck::from_bytes::<RayHitResult>(bytemuck::bytes_of(&rh)),
            rh
        );

        let ap = ArcCastParams {
            origin: Vec3 {
                x: 0.0,
                y: 1.6,
                z: 0.0,
            },
            dir: Vec3 {
                x: 0.0,
                y: 0.5,
                z: 1.0,
            },
            type_filter_hash: TYPE_HASH_ANY,
            speed: 8.0,
            gravity: 9.81,
            max_time: 3.0,
            sample_count: 24,
            flags: 0,
            purpose_mask: 0,
        };
        assert_eq!(
            *bytemuck::from_bytes::<ArcCastParams>(bytemuck::bytes_of(&ap)),
            ap
        );

        let ah = ArcHitResult {
            hit_entity: Entity::NULL,
            hit_position: Vec3::default(),
            hit_distance: 0.0,
            hit: 0,
            sample_count: 24,
        };
        assert_eq!(
            *bytemuck::from_bytes::<ArcHitResult>(bytemuck::bytes_of(&ah)),
            ah
        );
    }

    #[test]
    fn type_hash_is_stable_and_distinct() {
        // Deterministic FNV-1a; non-empty tokens never collide with "any".
        assert_ne!(type_hash("Mesh"), TYPE_HASH_ANY);
        assert_ne!(type_hash("DomeLight"), TYPE_HASH_ANY);
        assert_ne!(type_hash("Mesh"), type_hash("Cube"));
        assert_eq!(type_hash("Sphere"), type_hash("Sphere"));
    }

    #[test]
    fn zero_channel_mask_is_unfiltered_and_layout_is_stable() {
        // The ABI invariant: a `RaycastParams` whose `channel_mask` is zero —
        // exactly what pre-channel behaviour wasm leaves in the former padding
        // slot — must decode as `CHANNEL_ANY` (unfiltered). A fully-zeroed
        // params struct (as `Default`/`Zeroable` produce) therefore carries no
        // channel filter, so legacy casts are byte-for-byte unchanged.
        assert_eq!(CHANNEL_ANY, 0);
        let legacy = RaycastParams::default();
        assert_eq!(legacy.channel_mask, CHANNEL_ANY);

        // The field reuses tail padding, so the size (and thus `ABI_VERSION`) is
        // unchanged — a zeroed byte buffer still decodes to an unfiltered cast.
        assert_eq!(core::mem::size_of::<RaycastParams>(), 48);
        let zeroed = [0u8; 48];
        let decoded: RaycastParams = bytemuck::pod_read_unaligned(&zeroed);
        assert_eq!(decoded.channel_mask, CHANNEL_ANY);
        assert_eq!(decoded, RaycastParams::default());

        // Distinct channel bits, none equal to the unfiltered sentinel.
        assert_ne!(CHANNEL_PHYSICS, CHANNEL_ANY);
        assert_ne!(CHANNEL_INTERACTION, CHANNEL_ANY);
        assert_ne!(CHANNEL_PHYSICS, CHANNEL_INTERACTION);
    }

    #[test]
    fn vec3_glam_round_trip() {
        let g = glam::Vec3::new(1.5, -2.5, 3.25);
        let w: Vec3 = g.into();
        let back: glam::Vec3 = w.into();
        assert_eq!(g, back);
    }

    #[test]
    fn quat_glam_round_trip() {
        let g = glam::Quat::from_rotation_y(0.75);
        let w: Quat = g.into();
        let back: glam::Quat = w.into();
        assert!((g.x - back.x).abs() < 1e-6);
        assert!((g.y - back.y).abs() < 1e-6);
        assert!((g.z - back.z).abs() < 1e-6);
        assert!((g.w - back.w).abs() < 1e-6);
    }

    #[test]
    fn mat4_glam_round_trip() {
        let g = glam::Mat4::from_translation(glam::Vec3::new(1.0, 2.0, 3.0));
        let w: Mat4 = g.into();
        let back: glam::Mat4 = w.into();
        assert_eq!(g, back);
    }

    #[test]
    fn xform_composes_matrix() {
        let x = Xform::from_trs(
            glam::Vec3::new(1.0, 0.0, 0.0),
            glam::Quat::IDENTITY,
            glam::Vec3::ONE,
        );
        let m = x.to_matrix();
        assert_eq!(m.w_axis.truncate(), glam::Vec3::new(1.0, 0.0, 0.0));
    }

    #[test]
    fn error_code_round_trip() {
        for code in [
            ErrorCode::Ok,
            ErrorCode::StaleHandle,
            ErrorCode::NotFound,
            ErrorCode::TypeMismatch,
            ErrorCode::CapabilityDenied,
            ErrorCode::ReadOnly,
            ErrorCode::InvalidArgument,
        ] {
            assert_eq!(ErrorCode::from_u32(code.as_u32()), code);
        }
        assert_eq!(ErrorCode::from_u32(9999), ErrorCode::InvalidArgument);
    }

    #[test]
    fn input_phase_round_trip() {
        for p in [
            InputPhase::None,
            InputPhase::Pressed,
            InputPhase::Held,
            InputPhase::Released,
        ] {
            assert_eq!(InputPhase::from_u32(p.as_u32()), p);
        }
    }

    #[test]
    fn behavior_kind_round_trip() {
        for k in [BehaviorKind::Runtime, BehaviorKind::Editor] {
            assert_eq!(BehaviorKind::from_u32(k.as_u32()), k);
            assert_eq!(BehaviorKind::from_token(k.as_token()), k);
        }
        assert_eq!(BehaviorKind::from_token("bogus"), BehaviorKind::Runtime);
    }

    #[test]
    fn stage_event_round_trip() {
        for e in [
            StageEvent::Loaded,
            StageEvent::Unloading,
            StageEvent::PlayStart,
            StageEvent::PlayStop,
            StageEvent::Paused,
            StageEvent::Resumed,
        ] {
            assert_eq!(StageEvent::from_u32(e.as_u32()), e);
            assert_eq!(StageEvent::from_token(e.as_token()), e);
        }
        assert_eq!(StageEvent::from_u32(9999), StageEvent::Loaded);
        assert_eq!(StageEvent::from_token("bogus"), StageEvent::Loaded);
    }

    #[test]
    fn playback_command_kind_round_trip() {
        for k in [
            PlaybackCommandKind::Play,
            PlaybackCommandKind::Pause,
            PlaybackCommandKind::Stop,
            PlaybackCommandKind::Seek,
            PlaybackCommandKind::SetSpeed,
            PlaybackCommandKind::SetLooping,
        ] {
            assert_eq!(PlaybackCommandKind::from_u32(k.as_u32()), k);
            assert_eq!(PlaybackCommandKind::from_token(k.as_token()), k);
        }
        assert_eq!(
            PlaybackCommandKind::from_u32(9999),
            PlaybackCommandKind::Play
        );
        assert_eq!(
            PlaybackCommandKind::from_token("bogus"),
            PlaybackCommandKind::Play
        );
    }

    #[test]
    fn overlay_command_kind_round_trip() {
        for k in [
            OverlayCommandKind::DrawLine,
            OverlayCommandKind::DrawArc,
            OverlayCommandKind::Highlight,
            OverlayCommandKind::ClearHighlight,
            OverlayCommandKind::SetMaterialOverride,
            OverlayCommandKind::ClearMaterialOverride,
            OverlayCommandKind::SetPostprocessParam,
            OverlayCommandKind::HighlightTint,
            OverlayCommandKind::HighlightOutline,
        ] {
            assert_eq!(OverlayCommandKind::from_u32(k.as_u32()), k);
            assert_eq!(OverlayCommandKind::from_token(k.as_token()), k);
        }
        assert_eq!(
            OverlayCommandKind::from_u32(9999),
            OverlayCommandKind::DrawLine
        );
        assert_eq!(
            OverlayCommandKind::from_token("bogus"),
            OverlayCommandKind::DrawLine
        );
    }

    #[test]
    fn entity_null_and_bits() {
        assert!(Entity::NULL.is_null());
        let e = Entity::from_bits(0x0000_0002_0000_0005);
        assert!(!e.is_null());
        assert_eq!(e.to_bits(), 0x0000_0002_0000_0005);
    }

    #[test]
    fn xr_space_round_trip() {
        for s in [
            XrSpace::Hmd,
            XrSpace::LeftHand,
            XrSpace::RightHand,
            XrSpace::LeftAim,
            XrSpace::RightAim,
        ] {
            assert_eq!(XrSpace::from_u32(s.as_u32()), s);
        }
        // Stable wire values — guests compiled against an older ABI must keep
        // reading the same integers.
        assert_eq!(XrSpace::LeftAim.as_u32(), 3);
        assert_eq!(XrSpace::RightAim.as_u32(), 4);
        assert_eq!(XrSpace::from_u32(9999), XrSpace::Hmd);
    }

    #[test]
    fn xr_input_source_bitmask() {
        // NONE is empty; the stable wire bit values must not drift.
        assert_eq!(XrInputSource::NONE.bits(), 0);
        assert_eq!(XrInputSource::CONTROLLER.bits(), 1);
        assert_eq!(XrInputSource::HAND.bits(), 2);
        assert!(!XrInputSource::NONE.is_tracked());

        // Bits are independent and combine.
        let both = XrInputSource::NONE.with_controller(true).with_hand(true);
        assert!(both.has_controller() && both.has_hand() && both.is_tracked());
        assert_eq!(both.bits(), 0b11);

        // Builder clears individually.
        let hand_only = both.with_controller(false);
        assert!(!hand_only.has_controller() && hand_only.has_hand());

        // Round-trips through the wire, dropping unknown future bits.
        assert_eq!(XrInputSource::from_bits(both.bits()), both);
        assert_eq!(XrInputSource::from_bits(0xFFFF_FFFF), both);
    }

    #[test]
    fn xr_pose_result_is_pod_and_valid_flag() {
        let untracked = XrPoseResult::untracked();
        assert!(!untracked.is_valid());
        let tracked = XrPoseResult::tracked(Xform::default());
        assert!(tracked.is_valid());
        // POD round-trip.
        let bytes = bytemuck::bytes_of(&tracked);
        let back: XrPoseResult = *bytemuck::from_bytes(bytes);
        assert_eq!(tracked, back);
    }
}
