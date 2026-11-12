
use bevy_ecs::prelude::*;
use bevy_ecs::world::World;
use idtx_usd::StageService;

use crate::{
    components::{CollisionBodyComp, CollisionComp, PrimNode, RigidBodyComp},
    hydrator::{
        read_axial, read_bool_attribute, read_double_attribute, read_token_attribute,
        SceneHydrator,
    },
    schema_ext::{SchemaHydrationCtx, SchemaHydrator},
};

/// Built-in [`SchemaHydrator`] for the `OpenUSD`-standard `UsdPhysicsCollisionAPI`
/// (`physics:collisionEnabled`), with `UsdPhysicsMeshCollisionAPI`
/// (`physics:approximation`) and the reduced `IdtxCollisionAPI` addendum
/// (`idtx:collision:hull:*` / `:channel`), projecting [`CollisionComp`].
#[derive(Debug)]
pub struct CollisionSchemaHydrator;

/// Built-in [`SchemaHydrator`] for the `IdtxCollisionBodyAPI` marker schema,
/// projecting [`CollisionBodyComp`].
#[derive(Debug)]
pub struct CollisionBodySchemaHydrator;

/// Built-in [`SchemaHydrator`] for the `UsdPhysicsRigidBodyAPI` schema,
/// projecting [`RigidBodyComp`].
#[derive(Debug)]
pub struct RigidBodySchemaHydrator;

impl SchemaHydrator for CollisionSchemaHydrator {
    fn name(&self) -> &'static str {
        "collision"
    }

    /// Hydrate [`CollisionComp`] from the `OpenUSD`-standard `UsdPhysicsCollisionAPI`
    /// (`physics:collisionEnabled`), with `UsdPhysicsMeshCollisionAPI`
    /// (`physics:approximation`) and the reduced `IdtxCollisionAPI` addendum
    /// (`idtx:collision:hull:*` / `:channel`) read when present.
    ///
    /// The [`Populator`] interface keys on a prim's concrete type and has no
    /// per-attribute read, but a collision API schema applies to a prim of *any*
    /// type, so the work is a per-prim FFI scan here. One cheap
    /// `prim_has_api(PhysicsCollisionAPI)` call per entity gates the attribute
    /// reads, so non-collidable prims (the vast majority) cost exactly one scan
    /// each.
    ///
    /// Additive + reconciling: it (re)stamps `CollisionComp` wherever the standard
    /// collider schema is applied and **removes** a stale one where the schema (or
    /// a `convexHull`'s hull data) has since gone. Returns the number of
    /// components added, refreshed, or removed.
    ///
    /// [`Populator`]: crate::populator::Populator
    fn hydrate(&self, ctx: &mut SchemaHydrationCtx<'_>, service: &StageService) -> usize {
        use idtx_usd_extension::{
            Approximation, CollisionChannel, ATTR_CHANNEL, ATTR_HULL_POINTS, ATTR_HULL_TRIANGLES,
            ATTR_PHYSICS_APPROXIMATION, ATTR_PHYSICS_COLLISION_ENABLED, COLLISION_SCHEMA,
            PHYSICS_COLLISION_SCHEMA, PHYSICS_MESH_COLLISION_SCHEMA,
        };

        use crate::collision::{resolve_collision_shape, CollisionDims, CollisionShape};

        // Remove any stale CollisionComp from `entity` (shared reconcile path).
        let drop_collider = |world: &mut World, entity: Entity| -> bool {
            if world.get::<CollisionComp>(entity).is_some() {
                world.entity_mut(entity).remove::<CollisionComp>();
                true
            } else {
                false
            }
        };

        let mut changed = 0;
        for (entity, prim) in ctx.entity_prims() {
            // The collidable gate is the OpenUSD-standard PhysicsCollisionAPI; a
            // prim with only the (reduced) IdtxCollisionAPI addendum is NOT a
            // collider (legacy single-schema vocabulary is sunset).
            if !service
                .prim_has_api(prim, PHYSICS_COLLISION_SCHEMA)
                .unwrap_or(false)
            {
                if drop_collider(ctx.world, entity) {
                    changed += 1;
                }
                continue;
            }

            let has_mesh_collision = service
                .prim_has_api(prim, PHYSICS_MESH_COLLISION_SCHEMA)
                .unwrap_or(false);
            let has_idtx = service.prim_has_api(prim, COLLISION_SCHEMA).unwrap_or(false);

            // `physics:collisionEnabled` resolves its schema fallback (true) even
            // when unauthored.
            let enabled =
                read_bool_attribute(service, prim, ATTR_PHYSICS_COLLISION_ENABLED).unwrap_or(true);

            // `physics:approximation` lives only on UsdPhysicsMeshCollisionAPI.
            let approximation = if has_mesh_collision {
                read_token_attribute(service, prim, ATTR_PHYSICS_APPROXIMATION)
                    .and_then(|t| Approximation::from_token(&t))
                    .unwrap_or(Approximation::None)
            } else {
                Approximation::None
            };

            // Reduced IDTX addendum: pre-baked hull + IDTX query channels. Absent
            // when only the standard schemas are applied (channels default to All).
            let (hull_points, hull_triangles, channels) = if has_idtx {
                let hull_points = service
                    .eval_point3f_array(prim, ATTR_HULL_POINTS, 0.0, true)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                let hull_triangles = service
                    .eval_int_array(prim, ATTR_HULL_TRIANGLES, 0.0, true)
                    .unwrap_or_default();
                let channels = service
                    .eval_token_array(prim, ATTR_CHANNEL, 0.0, true)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|token| CollisionChannel::from_token(token))
                    .collect();
                (hull_points, hull_triangles, channels)
            } else {
                (Vec::new(), Vec::new(), vec![CollisionChannel::All])
            };

            // Resolve the narrow-phase shape from approximation + gprim type.
            let gprim_type = ctx
                .world
                .get::<PrimNode>(entity)
                .map_or_else(String::new, |n| n.type_name.clone());
            let shape = resolve_collision_shape(approximation, has_mesh_collision, &gprim_type);

            // `convexHull` with no pre-baked hull data is NON-COLLIDING: IDTX never
            // regenerates a hull at runtime.
            if matches!(approximation, Approximation::ConvexHull) && hull_points.is_empty() {
                if drop_collider(ctx.world, entity) {
                    changed += 1;
                }
                continue;
            }

            // Analytic dimensions sourced from the collider gprim's OWN geometry
            // (standard UsdGeom attrs). Each shape carries ONLY its own geometry
            // so a Cylinder's `radius` never leaks into `sphere_radius`. A mesh
            // boundingSphere/boundingCube has no analytic gprim geometry → the
            // dim stays `None` and resolve_analytic falls back to BoundsComp.
            let dims = match shape {
                CollisionShape::Sphere => CollisionDims {
                    sphere_radius: read_double_attribute(service, prim, "radius"),
                    ..CollisionDims::default()
                },
                CollisionShape::Cube => CollisionDims {
                    cube_size: read_double_attribute(service, prim, "size"),
                    ..CollisionDims::default()
                },
                CollisionShape::Cylinder => CollisionDims {
                    cylinder: read_axial(service, prim, false),
                    ..CollisionDims::default()
                },
                CollisionShape::Cone => CollisionDims {
                    cone: read_axial(service, prim, false),
                    ..CollisionDims::default()
                },
                CollisionShape::Capsule => CollisionDims {
                    // UsdGeomCapsule.height is the cylinder-segment length; the
                    // collider math wants the tip-to-tip half-extent
                    // (= segment + 2·radius), so convert for the capsule only.
                    capsule: read_axial(service, prim, true),
                    ..CollisionDims::default()
                },
                CollisionShape::Mesh | CollisionShape::Hull => CollisionDims::default(),
            };

            let next = CollisionComp {
                enabled,
                shape,
                approximation,
                dims,
                hull_points,
                hull_triangles,
                channels,
            };
            // Skip the write when nothing changed (avoids change-detection churn
            // on repeated hydration passes).
            if ctx.world.get::<CollisionComp>(entity) != Some(&next) {
                ctx.world.entity_mut(entity).insert(next);
                changed += 1;
            }
        }
        changed
    }
}

impl SchemaHydrator for CollisionBodySchemaHydrator {
    fn name(&self) -> &'static str {
        "collision-body"
    }

    /// Hydrate [`CollisionBodyComp`] from the `IdtxCollisionBodyAPI` marker
    /// schema — the whole-asset interaction/body grouping prim applied to an
    /// `Xformable`.
    ///
    /// A sibling pass to [`CollisionSchemaHydrator`] rather than part of it: the
    /// collider gate (`PhysicsCollisionAPI`) `continue`s past non-collider prims,
    /// but a body is normally a plain `Xform` with no collider of its own, so it
    /// must be probed independently. One `prim_has_api` scan per entity,
    /// inserting / removing the marker on change (so the pass doubles as the
    /// reconcile path after the API is removed). Returns the number of markers
    /// added or removed.
    fn hydrate(&self, ctx: &mut SchemaHydrationCtx<'_>, service: &StageService) -> usize {
        use idtx_usd_extension::COLLISION_BODY_SCHEMA;

        let mut changed = 0;
        for (entity, prim) in ctx.entity_prims() {
            let is_body = service
                .prim_has_api(prim, COLLISION_BODY_SCHEMA)
                .unwrap_or(false);
            let has = ctx.world.get::<CollisionBodyComp>(entity).is_some();
            if is_body && !has {
                ctx.world.entity_mut(entity).insert(CollisionBodyComp);
                changed += 1;
            } else if !is_body && has {
                ctx.world.entity_mut(entity).remove::<CollisionBodyComp>();
                changed += 1;
            }
        }
        changed
    }
}

impl SchemaHydrator for RigidBodySchemaHydrator {
    fn name(&self) -> &'static str {
        "rigid-body"
    }

    /// Hydrate [`RigidBodyComp`] from the `UsdPhysicsRigidBodyAPI` schema — the
    /// root of a simulated rigid body. The physics counterpart of
    /// [`CollisionBodySchemaHydrator`]: a rigid body is normally a plain `Xform`
    /// grouping child-gprim colliders, so it is probed independently of the
    /// collider gate. Lets a grab behavior resolve a grabbed child collider up to
    /// its body root ([`crate::rigid_body_of`]). One `prim_has_api` scan per
    /// entity, inserting / removing the marker on change (doubling as the
    /// reconcile path). Returns the number of markers added or removed.
    fn hydrate(&self, ctx: &mut SchemaHydrationCtx<'_>, service: &StageService) -> usize {
        use idtx_usd_extension::PHYSICS_RIGID_BODY_SCHEMA;

        let mut changed = 0;
        for (entity, prim) in ctx.entity_prims() {
            let is_body = service
                .prim_has_api(prim, PHYSICS_RIGID_BODY_SCHEMA)
                .unwrap_or(false);
            let has = ctx.world.get::<RigidBodyComp>(entity).is_some();
            if is_body && !has {
                ctx.world.entity_mut(entity).insert(RigidBodyComp);
                changed += 1;
            } else if !is_body && has {
                ctx.world.entity_mut(entity).remove::<RigidBodyComp>();
                changed += 1;
            }
        }
        changed
    }
}

// ── Thin inherent wrappers (preserve the existing public method surface) ────────

impl SceneHydrator {
    /// Delegates to [`CollisionSchemaHydrator`]. See its
    /// [`SchemaHydrator::hydrate`] for the full contract.
    pub fn hydrate_collision(&mut self, service: &StageService) -> usize {
        let mut ctx = self.schema_ctx();
        CollisionSchemaHydrator.hydrate(&mut ctx, service)
    }

    /// Delegates to [`CollisionBodySchemaHydrator`].
    pub fn hydrate_collision_bodies(&mut self, service: &StageService) -> usize {
        let mut ctx = self.schema_ctx();
        CollisionBodySchemaHydrator.hydrate(&mut ctx, service)
    }

    /// Delegates to [`RigidBodySchemaHydrator`].
    pub fn hydrate_rigid_bodies(&mut self, service: &StageService) -> usize {
        let mut ctx = self.schema_ctx();
        RigidBodySchemaHydrator.hydrate(&mut ctx, service)
    }
}
