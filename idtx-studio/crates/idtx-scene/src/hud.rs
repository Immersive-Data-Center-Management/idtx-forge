//! ECS populator for the `IdtxHud` concrete typed schema. Actual attribute
//! hydration is handled by
//! [`crate::hydrator::SceneHydrator::hydrate_hud`]; this module only registers
//! the type name for the [`crate::populator::PopulatorRegistry`] so that
//! `depopulate` removes a stale [`HudComp`] when a prim's type changes.

use bevy_ecs::prelude::*;
use idtx_usd::{PrimId, ViewportProjection};

use crate::components::HudComp;
use crate::populator::Populator;

/// USD type name string registered by [`HudPopulator`].
pub const HUD_TYPE_NAME: &str = "IdtxHud";

/// Populator for `IdtxHud` typed prims.
///
/// `populate()` is a no-op because [`ViewportProjection`] carries no
/// `idtx:hud:*` attribute values; actual component hydration runs via
/// [`crate::hydrator::SceneHydrator::hydrate_hud`]. The populator's role is to
/// register the type name and to clean up via `depopulate` when the prim's
/// schema type is replaced.
#[derive(Debug)]
pub struct HudPopulator;

impl Populator for HudPopulator {
    fn type_names(&self) -> &[&'static str] {
        &[HUD_TYPE_NAME]
    }

    fn populate(
        &self,
        _entity: &mut EntityWorldMut<'_>,
        _prim: PrimId,
        _projection: &ViewportProjection,
    ) {
        // No per-attribute data is available from the viewport projection for
        // IdtxHud prims. hydrate_hud() reads the actual attribute values.
    }

    fn depopulate(&self, entity: &mut EntityWorldMut<'_>) {
        entity.remove::<HudComp>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::world::World;

    #[test]
    fn hud_populator_type_names() {
        let pop = HudPopulator;
        assert_eq!(pop.type_names(), &["IdtxHud"]);
    }

    #[test]
    fn hud_populator_depopulate_removes_comp() {
        use idtx_usd_extension::HudAnchor;
        let mut world = World::new();
        let e = world
            .spawn(HudComp {
                image: "crosshair.png".to_owned(),
                size_px: [0, 0],
                anchor: HudAnchor::Center,
                opacity: 1.0,
            })
            .id();
        let pop = HudPopulator;
        {
            let mut em = world.entity_mut(e);
            pop.depopulate(&mut em);
        }
        assert!(world.get::<HudComp>(e).is_none(), "depopulate must remove HudComp");
    }
}
