//! Extension point for custom USD schema hydration.
//!
//! Built-in USD schema types (Mesh, Camera, lights, etc.) are handled directly
//! by [`crate::hydrator::SceneHydrator`]. The [`PopulatorRegistry`] provides an
//! escape hatch for *non-standard* schema types: community crates can register
//! custom [`Populator`] implementations at startup, and the hydrator will call
//! them for any prim whose `type_name` matches the registration.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use bevy_ecs::prelude::*;
use idtx_usd::{PrimId, ViewportProjection};

// ── Populator trait ───────────────────────────────────────────────────────────

/// Hydrates ECS components for one family of USD schema types.
///
/// Implement this trait in a downstream crate and register it with
/// [`PopulatorRegistry::register`] before the first stage opens.
///
/// # Thread safety
///
/// Populators are stored behind `Arc` and may be shared across threads.
/// The `populate` / `depopulate` calls always arrive on the hydrator's thread
/// with an exclusive `EntityWorldMut` borrow, so interior mutability is not
/// required.
pub trait Populator: Send + Sync + fmt::Debug {
    /// USD schema `type_name` strings this populator handles.
    ///
    /// Return an empty slice to register as the global fallback populator —
    /// called for every prim whose type name has no specific registration.
    /// Only one fallback is allowed; the last registered wins.
    fn type_names(&self) -> &[&'static str];

    /// Insert or update components for a prim that appeared in the viewport
    /// projection with this populator's schema type.
    ///
    /// The full `ViewportProjection` is supplied so populators can resolve
    /// cross-prim references (e.g. material bindings). The `eval_attribute`
    /// FFI escape hatch (Phase 6) will wire per-attribute access.
    fn populate(
        &self,
        entity: &mut EntityWorldMut<'_>,
        prim: PrimId,
        projection: &ViewportProjection,
    );

    /// Remove all components inserted by this populator when the prim is
    /// despawned or replaced by a different schema type.
    fn depopulate(&self, entity: &mut EntityWorldMut<'_>);
}

// ── PopulatorRegistry ─────────────────────────────────────────────────────────

/// Maps USD schema type names to registered [`Populator`] implementations.
///
/// The registry is populated at application startup before any stage opens.
/// After that it is read-only during the session lifetime.
#[derive(Debug, Default)]
pub struct PopulatorRegistry {
    by_type: HashMap<String, Arc<dyn Populator>>,
    fallback: Option<Arc<dyn Populator>>,
}

impl PopulatorRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a populator. For populators with an empty `type_names()` slice
    /// this becomes the fallback handler for all unrecognised schema types;
    /// only the last-registered fallback is retained.
    pub fn register(&mut self, populator: Arc<dyn Populator>) {
        if populator.type_names().is_empty() {
            self.fallback = Some(populator);
        } else {
            for &type_name in populator.type_names() {
                self.by_type
                    .insert(type_name.to_owned(), Arc::clone(&populator));
            }
        }
    }

    /// Returns the populator for `type_name`, or the fallback if no specific
    /// handler is registered. Returns `None` only when the registry is empty
    /// and there is no fallback.
    #[must_use]
    pub fn get(&self, type_name: &str) -> Option<&Arc<dyn Populator>> {
        self.by_type.get(type_name).or(self.fallback.as_ref())
    }

    /// Returns the number of registered type-specific populators (excluding
    /// the fallback).
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_type.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_type.is_empty() && self.fallback.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct MockPopulator(&'static str);

    impl Populator for MockPopulator {
        fn type_names(&self) -> &[&'static str] {
            std::slice::from_ref(&self.0)
        }
        fn populate(&self, _: &mut EntityWorldMut<'_>, _: PrimId, _: &ViewportProjection) {}
        fn depopulate(&self, _: &mut EntityWorldMut<'_>) {}
    }

    #[test]
    fn registry_lookup_and_fallback() {
        let mut registry = PopulatorRegistry::new();
        assert!(registry.get("Mesh").is_none());

        registry.register(Arc::new(MockPopulator("Mesh")));
        assert!(registry.get("Mesh").is_some());
        assert!(registry.get("Camera").is_none());

        #[derive(Debug)]
        struct Fallback;
        impl Populator for Fallback {
            fn type_names(&self) -> &[&'static str] {
                &[]
            }
            fn populate(&self, _: &mut EntityWorldMut<'_>, _: PrimId, _: &ViewportProjection) {}
            fn depopulate(&self, _: &mut EntityWorldMut<'_>) {}
        }

        registry.register(Arc::new(Fallback));
        assert!(registry.get("Camera").is_some()); // falls back
        assert!(registry.get("Mesh").is_some()); // specific still wins
    }
}
