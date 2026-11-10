//! ECS populator for the `IdtxWidget*` concrete-typed schemas (Phase 2 of
//! docs/widgets-in-usd-proposal-v2.md). Actual attribute hydration is handled by
//! [`crate::hydrator::SceneHydrator::hydrate_widgets`]; this module only
//! registers the widget type names with the
//! [`crate::populator::PopulatorRegistry`] so that `depopulate` removes a stale
//! [`WidgetPanelComp`] when a prim's type changes.

use bevy_ecs::prelude::*;
use idtx_usd::{PrimId, ViewportProjection};
use idtx_usd_extension::WIDGET_SCHEMAS;

use crate::components::WidgetPanelComp;
use crate::populator::Populator;

/// Populator for the `IdtxWidget*` typed prims (panel + element schemas).
///
/// `populate()` is a no-op because [`ViewportProjection`] carries no
/// `idtx:widget:*` attribute values; the real component hydration (reading
/// attributes + walking the element tree) runs via
/// [`crate::hydrator::SceneHydrator::hydrate_widgets`]. The populator's role is
/// to register the widget type names and to clean up via `depopulate` when a
/// prim's schema type is replaced.
#[derive(Debug)]
pub struct WidgetPopulator;

impl Populator for WidgetPopulator {
    fn type_names(&self) -> &[&'static str] {
        &WIDGET_SCHEMAS
    }

    fn populate(
        &self,
        _entity: &mut EntityWorldMut<'_>,
        _prim: PrimId,
        _projection: &ViewportProjection,
    ) {
        // No per-attribute data is available from the viewport projection for
        // IdtxWidget* prims. hydrate_widgets() reads the actual attribute values.
    }

    fn depopulate(&self, entity: &mut EntityWorldMut<'_>) {
        // Only panels carry WidgetPanelComp; removing it from an element entity
        // is a harmless no-op.
        entity.remove::<WidgetPanelComp>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::world::World;
    use idtx_usd_extension::{WidgetBillboard, WidgetSpace, WIDGET_PANEL_SCHEMA};

    #[test]
    fn widget_populator_registers_all_widget_types() {
        let pop = WidgetPopulator;
        assert_eq!(pop.type_names().len(), 11);
        assert!(pop.type_names().contains(&WIDGET_PANEL_SCHEMA));
        assert!(pop.type_names().contains(&idtx_usd_extension::WIDGET_FIELD_DOUBLE_SCHEMA));
        assert!(pop.type_names().contains(&idtx_usd_extension::WIDGET_FIELD_STRING_SCHEMA));
    }

    #[test]
    fn widget_populator_depopulate_removes_panel_comp() {
        let mut world = World::new();
        let e = world
            .spawn(WidgetPanelComp {
                size: [0.4, 0.3],
                resolution: [512, 512],
                pixels_per_unit: 2048.0,
                billboard: WidgetBillboard::None,
                double_sided: true,
                opacity: 1.0,
                space: WidgetSpace::World,
                content_source: None,
                elements: Vec::new(),
                content_hash: 0,
            })
            .id();
        let pop = WidgetPopulator;
        {
            let mut em = world.entity_mut(e);
            pop.depopulate(&mut em);
        }
        assert!(
            world.get::<WidgetPanelComp>(e).is_none(),
            "depopulate must remove WidgetPanelComp"
        );
    }
}
