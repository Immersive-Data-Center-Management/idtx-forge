//! `Ctx` — the per-hook context handed to author code.
//!
//! Exposes the self entity, frame delta-time, current time-code, and the
//! XR pose accessor. All reads route through the host import table on wasm
//! and return defaults natively.

use crate::abi::Entity as WireEntity;
use crate::entity::Entity;
use crate::events::Event;
use crate::stage_edit::StageEdit;
use crate::xr::XrHmd;

/// Per-hook context.
///
/// Constructed by the SDK runtime glue immediately before each hook fires
/// (see [`crate::__rt::dispatch`]). Cheap to copy — it holds only the self
/// handle; `dt` / `time_code` are pulled lazily from the host.
#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    self_entity: WireEntity,
}

impl Ctx {
    /// Build the context for the current hook by asking the host which
    /// entity the behavior is attached to.
    #[must_use]
    pub fn current() -> Self {
        #[cfg(target_arch = "wasm32")]
        let bits = unsafe { crate::__rt::idtx_entity_self() };
        #[cfg(not(target_arch = "wasm32"))]
        let bits = 0u64;
        Self {
            self_entity: WireEntity::from_bits(bits),
        }
    }

    /// The entity this behavior is attached to.
    ///
    /// For a behavior attached to the stage pseudo-root this is
    /// [`Entity::is_null`]; use [`Entity::find`] to reach concrete prims.
    #[must_use]
    pub fn entity(&self) -> Entity {
        Entity::from_wire(self.self_entity)
    }

    /// Frame delta-time in seconds (the value passed to `on_update`).
    #[must_use]
    pub fn delta_seconds(&self) -> f32 {
        #[cfg(target_arch = "wasm32")]
        {
            unsafe { crate::__rt::idtx_delta_seconds() }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            0.0
        }
    }

    /// The stage's current time-code.
    #[must_use]
    pub fn time_code(&self) -> f64 {
        #[cfg(target_arch = "wasm32")]
        {
            unsafe { crate::__rt::idtx_time_code() }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            0.0
        }
    }

    /// XR pose accessor (requires the `xr_hmd` capability). Reads the most
    /// recently polled tracked-space poses (HMD / controllers).
    #[must_use]
    pub fn xr(&self) -> XrHmd {
        XrHmd::new()
    }

    /// The event currently being delivered (requires the `events`
    /// capability). Only meaningful inside `on_event`; outside it the
    /// handle reads back an empty/null event.
    #[must_use]
    pub fn event(&self) -> Event {
        Event::current()
    }

    /// Named-event **emission** surface (requires the `events` capability).
    /// `ctx.events().broadcast("activated", &[])` posts a deferred event the
    /// host delivers to matching `on_event` hooks after the update hooks. The
    /// read side of a delivered event is [`Ctx::event`].
    #[must_use]
    pub fn events(&self) -> crate::events::Events {
        crate::events::Events::current()
    }

    /// Clip playback transport (requires the `playback` capability).
    /// `ctx.animations().clip("open").play()` posts a deferred command the
    /// host applies to the addressed playhead after the update hooks. Address a
    /// clip on the caller's own binding with
    /// [`clip`](crate::animations::Animations::clip), or another entity with
    /// [`clip_on`](crate::animations::Animations::clip_on).
    #[must_use]
    pub fn animations(&self) -> crate::animations::Animations {
        crate::animations::Animations::current()
    }

    /// Read this binding's immutable authored context into a typed contract.
    /// Values are pinned before attachment; the SDK handles decoding and bounds.
    pub fn context<'a, T: serde::Deserialize<'a>>(
        &self,
        scratch: &'a mut [u8],
    ) -> crate::result::Result<T> {
        crate::context::own(scratch)
    }

    /// Deferred session-layer authoring surface (requires the `stage_edit`
    /// capability). Each call enqueues an edit the host applies + re-projects
    /// after the update hooks — never an in-hook USD mutation.
    #[must_use]
    pub fn stage_edit(&self) -> StageEdit {
        StageEdit::current()
    }

    /// Scene-interaction surface (raycast / arc-cast / prim-type query +
    /// highlight). Requires the `scene_query` capability for queries and
    /// `overlay` for highlighting.
    #[must_use]
    pub fn scene(&self) -> crate::scene::Scene {
        crate::scene::Scene::current()
    }

    /// Deferred level-transition surface (requires the `level_control`
    /// capability). `ctx.level().goto("cave")` enqueues a request the runtime
    /// driver performs after the update hooks — never an in-hook session swap.
    #[must_use]
    pub fn level(&self) -> crate::level::Level {
        crate::level::Level::current()
    }

    /// Runtime physics control (requires the `physics` capability): suspend /
    /// resume simulation of a body for grabbing. Flip a grabbed dynamic prim to
    /// kinematic while held so the authored pose is not fought by the solver,
    /// then resume on release. See [`crate::physics::Physics`].
    #[must_use]
    pub fn physics(&self) -> crate::physics::Physics {
        crate::physics::Physics::current()
    }

    /// Transient per-prim highlight / overlay-material surface (requires the
    /// `overlay` capability). Shorthand for `self.scene().highlight()`.
    #[must_use]
    pub fn highlight(&self) -> crate::scene::Highlight {
        crate::scene::Highlight::current()
    }

    /// Transient world-space overlay-draw surface (requires the `overlay`
    /// capability): lines, polylines, and ring markers drawn over the beauty
    /// image for one frame — the teleport pointer arc + landing marker. Shorthand
    /// for `self.scene().draw()`.
    #[must_use]
    pub fn draw(&self) -> crate::scene::Draw {
        crate::scene::Draw::current()
    }

    /// Panel-side widget surface (requires the `widget` capability). For a
    /// behavior attached to an `IdtxWidgetPanel`: register click handlers for the
    /// panel's child elements in `on_start` (e.g.
    /// `ctx.widget().on_click("StartButton", Self::start_clicked)?`). The
    /// handler fires via the generated `_idtx_on_event` dispatcher when a pointer
    /// behavior clicks that element — no `on_event` boilerplate.
    #[must_use]
    pub fn widget(&self) -> crate::scene::Widget {
        crate::scene::Widget::current()
    }

    /// Pointer-side widget surface (requires the `widget` capability). For a
    /// gaze/pointer behavior: hand a widget raycast hit (from a
    /// [`block_widgets`](crate::scene::RayOpts::block_widgets) cast) to the
    /// host's built-in interaction processing via `ctx.widgets().handle(hit)`,
    /// which draws the hit cue, dispatches the click, and reports whether the
    /// widget consumed the interaction.
    #[must_use]
    pub fn widgets(&self) -> crate::scene::Widgets {
        crate::scene::Widgets::current()
    }

    /// The active view/player camera this frame — the desktop orbit/player
    /// camera, or the HMD in XR. `None` when no camera pose is available.
    ///
    /// Returns both the camera as a ray (`cam.ray`) and the camera's full
    /// world transform (`cam.transform`, stage space). Use the ray to cast
    /// from what the player is looking at, e.g. a desktop gaze-pick:
    /// `ctx.scene().raycast(cam.ray.origin, cam.ray.forward, &opts)`; use the
    /// transform to rigidly attach a grabbed prim to the camera frame.
    #[must_use]
    pub fn camera(&self) -> Option<CameraView> {
        #[allow(unused_mut)] // written via raw ptr on wasm; via copy natively
        let mut buf = [0u8; core::mem::size_of::<crate::abi::CameraPose>()];
        #[cfg(target_arch = "wasm32")]
        unsafe {
            crate::__rt::idtx_camera_pose(buf.as_mut_ptr());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Natively: no camera available (keeps author logic testable).
            buf.copy_from_slice(bytemuck::bytes_of(&crate::abi::CameraPose::default()));
        }
        let pose: crate::abi::CameraPose = bytemuck::pod_read_unaligned(&buf);
        if pose.valid == 0 {
            return None;
        }
        // Pull the full camera world transform from the sibling import (only
        // read when the pose is valid; identity otherwise, but unused).
        #[allow(unused_mut)] // written via raw ptr on wasm; via copy natively
        let mut xbuf = [0u8; core::mem::size_of::<crate::abi::Mat4>()];
        #[cfg(target_arch = "wasm32")]
        unsafe {
            crate::__rt::idtx_camera_transform(xbuf.as_mut_ptr());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            xbuf.copy_from_slice(bytemuck::bytes_of(&crate::abi::Mat4::default()));
        }
        let transform: crate::abi::Mat4 = bytemuck::pod_read_unaligned(&xbuf);
        Some(CameraView {
            ray: CameraRay {
                origin: pose.position.into(),
                forward: pose.forward.into(),
                up: pose.up.into(),
            },
            transform: transform.into(),
        })
    }
}

/// The active view/player camera this frame: a world-space ray plus the
/// camera's full world transform (both stage space).
#[derive(Clone, Copy, Debug)]
pub struct CameraView {
    /// The camera as a world-space ray (origin / forward / up).
    pub ray: CameraRay,
    /// The camera's full world transform — its frame in stage space. Use it to
    /// rigidly attach a grabbed prim (`camera^-1 * prim_world` offset), the
    /// desktop analogue of an XR controller matrix.
    pub transform: glam::Mat4,
}

/// The active view/player camera as a world-space ray (stage space).
#[derive(Clone, Copy, Debug)]
pub struct CameraRay {
    /// Camera world-space position (ray origin).
    pub origin: glam::Vec3,
    /// Normalized look direction (ray direction).
    pub forward: glam::Vec3,
    /// Camera up vector.
    pub up: glam::Vec3,
}
