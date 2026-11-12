//! Runtime extension point for USD-extension *schema hydration*.
//!
//! Built-in USD schema types (Mesh, Camera, lights, …) and the IDTX schema
//! domains (collision/physics, HUD, widgets) are projected into ECS components
//! by [`crate::hydrator::SceneHydrator`]. Those IDTX domains are compiled in
//! behind the `usd-extension` feature. This module is the seam that lets a USD
//! extension added *after* `idtx-scene` is compiled — its codeless schemas
//! registered at runtime with [`idtx_usd_extension::install_at`] — also supply
//! its Rust-side ECS projection, by registering a [`SchemaHydrator`] with the
//! [`SchemaHydratorRegistry`] a [`SceneHydrator`] owns. (Its codeless schemas
//! are registered at runtime with `idtx_usd_extension::install_at`.)
//!
//! This is the per-prim-FFI-scan counterpart to [`crate::populator`]: a
//! [`crate::populator::Populator`] keys on a prim's concrete `type_name` and
//! hydrates from the already-projected [`idtx_usd::ViewportProjection`], whereas
//! a [`SchemaHydrator`] runs its own reconcile pass against the live
//! [`idtx_usd::StageService`] (so it can probe `HasAPI` schemas and read
//! arbitrary authored attributes, which the projection does not carry).
//!
//! The plugin-facing API surface is: this trait, [`SchemaHydrationCtx`] (world +
//! entity access), and the `pub` `read_*` attribute helpers in
//! [`crate::hydrator`].
//!
//! # Follow-up: loading extensions after compilation
//!
//! This change wires the registry and registers the built-in domains as
//! compiled-in `Arc<dyn SchemaHydrator>`s. The remaining steps to load a USD
//! extension that did not exist when `idtx-scene` was built (out of scope here):
//!
//! 1. the host discovers a cooked extension directory and calls
//!    `idtx_usd_extension::install_at(dir)` to register its codeless schemas with
//!    the `OpenUSD` plugin registry (so `prim_has_api` / `prim_is_a` /
//!    `eval_attribute` resolve the new `idtx:*` content);
//! 2. the host obtains a matching [`SchemaHydrator`] and calls
//!    [`SceneHydrator::register_schema_hydrator`] so the new schema is projected
//!    into ECS components each drain.
//!
//! Step 2's hydrator is a compiled-in `Arc`. The trait, [`SchemaHydrationCtx`],
//! and the `pub` `read_*` helpers are the stable surface a dynamic library
//! exposing a C-ABI entry point (`fn register(&mut SchemaHydratorRegistry)`)
//! links against.
//!
//! [`SceneHydrator`]: crate::hydrator::SceneHydrator
//! [`SceneHydrator::register_schema_hydrator`]: crate::hydrator::SceneHydrator::register_schema_hydrator

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use bevy_ecs::prelude::*;
use bevy_ecs::world::World;
use idtx_usd::{PrimId, StageService};

// ── SchemaHydrator trait ───────────────────────────────────────────────────────

/// Hydrates ECS components for one family of USD-extension schemas by scanning
/// the hydrated world against a live [`StageService`].
///
/// Implement this in a downstream crate (or a runtime plugin) and register it
/// with [`SchemaHydratorRegistry::register`] / [`SceneHydrator::register_schema_hydrator`]
/// before the drain loop runs.
///
/// # Contract
///
/// `hydrate` is an **additive + reconciling** full pass, matching the built-in
/// hydrators: it (re)stamps its components wherever its schema is applied and
/// **removes** stale ones where the schema has since gone, so one call doubles as
/// the reconcile path after an authoring edit or a root-layer swap. It returns
/// the number of components added, refreshed, or removed.
///
/// # Thread safety
///
/// Hydrators are stored behind `Arc` and may be shared across threads. `hydrate`
/// always arrives on the hydrator's thread with an exclusive world borrow via
/// [`SchemaHydrationCtx`], so interior mutability is not required.
///
/// [`SceneHydrator::register_schema_hydrator`]: crate::hydrator::SceneHydrator::register_schema_hydrator
pub trait SchemaHydrator: Send + Sync + fmt::Debug {
    /// Short diagnostic name for the schema family (e.g. `"collision"`, `"hud"`,
    /// or a plugin's own identifier). Used only for logging / introspection.
    fn name(&self) -> &str;

    /// Run one reconcile pass. See the trait-level contract. Returns the number
    /// of components added, refreshed, or removed.
    fn hydrate(&self, ctx: &mut SchemaHydrationCtx<'_>, service: &StageService) -> usize;
}

// ── SchemaHydrationCtx ──────────────────────────────────────────────────────────

/// The world + entity-index access a [`SchemaHydrator`] operates through.
///
/// Borrows the [`SceneHydrator`](crate::hydrator::SceneHydrator)'s `World` and its
/// `PrimId → Entity` index for the duration of one pass. Combine
/// [`Self::entity_prims`] (the snapshot every built-in pass iterates) with the
/// `pub` `read_*` helpers in [`crate::hydrator`] to read authored attributes.
pub struct SchemaHydrationCtx<'a> {
    /// The hydrated ECS world. Insert/remove components here.
    pub world: &'a mut World,
    /// `PrimId → Entity` index, mirrored from the owning hydrator.
    entities: &'a HashMap<PrimId, Entity>,
}

impl<'a> SchemaHydrationCtx<'a> {
    /// Construct a context over a world and its entity index. Called by
    /// [`SceneHydrator::run_schema_hydrators`](crate::hydrator::SceneHydrator::run_schema_hydrators).
    #[must_use]
    pub fn new(world: &'a mut World, entities: &'a HashMap<PrimId, Entity>) -> Self {
        Self { world, entities }
    }

    /// Snapshot of every `(entity, prim)` currently in the topology world — the
    /// list every reconcile pass iterates. Collected up front so the pass can
    /// mutate the world without re-borrowing the index.
    #[must_use]
    pub fn entity_prims(&self) -> Vec<(Entity, PrimId)> {
        self.entities.iter().map(|(&prim, &entity)| (entity, prim)).collect()
    }

    /// The ECS entity for `prim`, if it is in the topology world.
    #[must_use]
    pub fn entity_for(&self, prim: PrimId) -> Option<Entity> {
        self.entities.get(&prim).copied()
    }
}

// ── SchemaHydratorRegistry ──────────────────────────────────────────────────────

/// Ordered set of [`SchemaHydrator`]s a [`SceneHydrator`](crate::hydrator::SceneHydrator)
/// runs each drain. Populated at startup (with the built-ins, plus any
/// runtime-registered plugin hydrators) and read-only during the session.
#[derive(Default, Clone)]
pub struct SchemaHydratorRegistry {
    hydrators: Vec<Arc<dyn SchemaHydrator>>,
}

impl fmt::Debug for SchemaHydratorRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SchemaHydratorRegistry")
            .field("names", &self.hydrators.iter().map(|h| h.name()).collect::<Vec<_>>())
            .finish()
    }
}

impl SchemaHydratorRegistry {
    /// Creates an empty registry (no hydrators — a core build with no IDTX
    /// schema domains compiled in, awaiting runtime registration).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry pre-loaded with the built-in collision / collision-body /
    /// rigid-body / HUD / widget hydrators. Only available when the
    /// `usd-extension` feature is on.
    #[cfg(feature = "usd-extension")]
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut reg = Self::new();
        reg.register(Arc::new(crate::hydrator::collision::CollisionSchemaHydrator));
        reg.register(Arc::new(crate::hydrator::collision::CollisionBodySchemaHydrator));
        reg.register(Arc::new(crate::hydrator::collision::RigidBodySchemaHydrator));
        reg.register(Arc::new(crate::hydrator::widget::HudSchemaHydrator));
        reg.register(Arc::new(crate::hydrator::widget::WidgetSchemaHydrator));
        reg
    }

    /// Appends a hydrator. Order is preserved; passes run in registration order.
    pub fn register(&mut self, hydrator: Arc<dyn SchemaHydrator>) {
        self.hydrators.push(hydrator);
    }

    /// Iterates the registered hydrators in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn SchemaHydrator>> {
        self.hydrators.iter()
    }

    /// Number of registered hydrators.
    #[must_use]
    pub fn len(&self) -> usize {
        self.hydrators.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hydrators.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Noop(&'static str);
    impl SchemaHydrator for Noop {
        fn name(&self) -> &str {
            self.0
        }
        fn hydrate(&self, _ctx: &mut SchemaHydrationCtx<'_>, _service: &StageService) -> usize {
            0
        }
    }

    #[test]
    fn registry_registers_and_preserves_order() {
        let mut reg = SchemaHydratorRegistry::new();
        assert!(reg.is_empty());
        reg.register(Arc::new(Noop("a")));
        reg.register(Arc::new(Noop("b")));
        assert_eq!(reg.len(), 2);
        let names: Vec<&str> = reg.iter().map(|h| h.name()).collect();
        assert_eq!(names, ["a", "b"]);
    }

    #[cfg(feature = "usd-extension")]
    #[test]
    fn builtins_register_the_five_domains() {
        let reg = SchemaHydratorRegistry::with_builtins();
        let names: Vec<&str> = reg.iter().map(|h| h.name()).collect();
        assert_eq!(names, ["collision", "collision-body", "rigid-body", "hud", "widget"]);
    }
}
