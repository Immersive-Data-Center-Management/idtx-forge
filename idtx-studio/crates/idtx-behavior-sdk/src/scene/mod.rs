//! Scene-interaction surface, clustered for discoverability.
//!
//! Two sub-namespaces group the host functions a behavior uses to interact
//! with the live scene:
//!
//! - [`query`] — [`raycast`](query::raycast), [`arc_cast`](query::arc_cast),
//!   and [`find_by_type`](query::find_by_type) (capability `scene_query`).
//! - [`highlight`] — per-prim outline / overlay-material commands
//!   (capability `overlay`).
//!
//! Use the free functions directly, or the [`Scene`] handle from
//! [`crate::ctx::Ctx::scene`] which groups them:
//!
//! ```ignore
//! let scene = ctx.scene();
//! if let Some(hit) = scene.raycast(origin, dir, &RayOpts::new().of_type("Mesh")) {
//!     scene.highlight().highlight(hit.entity, Vec4::new(1.0, 0.6, 0.0, 1.0))?;
//! }
//! ```

pub mod draw;
pub mod highlight;
pub mod query;
pub mod widget;

pub use draw::Draw;
pub use highlight::Highlight;
pub use query::{
    ArcHit, ArcLaunch, PrimPath, RayHit, RayOpts, arc_cast, collision_body, find_by_type, raycast,
};
pub use widget::{
    Widget, WidgetClick, WidgetHandler, WidgetHandlerRegistry, WidgetHandlerSlot, Widgets,
};

use crate::entity::Entity;
use glam::Vec3;

/// Grouping handle over the scene-interaction functions, returned by
/// [`crate::ctx::Ctx::scene`]. Cheap to copy — holds no state.
#[derive(Clone, Copy, Debug)]
pub struct Scene {
    _private: (),
}

impl Scene {
    #[must_use]
    pub(crate) fn current() -> Self {
        Self { _private: () }
    }

    /// Cast a ray into the scene (see [`query::raycast`]).
    #[must_use]
    pub fn raycast(self, origin: Vec3, dir: Vec3, opts: &RayOpts) -> Option<RayHit> {
        query::raycast(origin, dir, opts)
    }

    /// Cast a ballistic arc (see [`query::arc_cast`]).
    pub fn arc_cast(self, launch: &ArcLaunch, opts: &RayOpts, out_points: &mut [Vec3]) -> ArcHit {
        query::arc_cast(launch, opts, out_points)
    }

    /// Enumerate prims of a USD type (see [`query::find_by_type`]).
    pub fn find_by_type<F: FnMut(&str, Entity)>(self, token: &str, f: F) -> u32 {
        query::find_by_type(token, f)
    }

    /// Resolve a hit prim up to its whole-asset collision body (see
    /// [`query::collision_body`]).
    #[must_use]
    pub fn collision_body(self, entity: Entity) -> Option<Entity> {
        query::collision_body(entity)
    }

    /// The transient highlight / overlay-material surface (see [`Highlight`]).
    #[must_use]
    pub fn highlight(self) -> Highlight {
        Highlight::current()
    }

    /// The transient world-space overlay-draw surface (lines / polylines / ring
    /// markers — the teleport arc + landing marker). See [`Draw`].
    #[must_use]
    pub fn draw(self) -> Draw {
        Draw::current()
    }
}
