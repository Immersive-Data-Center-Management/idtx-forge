use egui::Ui;

use crate::inspector::{
    InspectorContext, InspectorContribution, PropertyClaimSet, widgets::attribute_row,
};

/// USD schema defaults for `UsdGeomCone`.
const DEFAULT_RADIUS: f64 = 0.5;
const DEFAULT_HEIGHT: f64 = 2.0;

const ATTR_RADIUS: &str = "radius";
const ATTR_HEIGHT: &str = "height";
const ATTR_AXIS: &str = "axis";
const ATTR_EXTENT: &str = "extent";

/// "Cone" section — unique `UsdGeomCone` attributes.
///
/// | Attribute | USD type | Meaning |
/// |-----------|----------|---------|
/// | `radius`  | `double` | Base radius of the cone. |
/// | `height`  | `double` | Height of the cone along its spine axis. |
/// | `axis`    | `token`  | Spine axis: `"X"`, `"Y"`, or `"Z"` (default). |
///
/// Common `UsdGeomGprim` attributes are rendered by [`MeshPanel`].
#[derive(Debug)]
pub struct ConePanel;

impl InspectorContribution for ConePanel {
    fn label(&self) -> &'static str {
        "Cone"
    }
    fn priority(&self) -> i32 {
        5
    }

    fn claim(&self, claims: &mut PropertyClaimSet) {
        claims.claim_editable(ATTR_RADIUS);
        claims.claim_editable(ATTR_HEIGHT);
        claims.claim_editable(ATTR_AXIS);
        claims.claim(ATTR_EXTENT);
    }

    fn present(&self, ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        ctx.type_name() == "Cone"
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, claims: &PropertyClaimSet) {
        let props: Vec<_> = ctx
            .properties
            .iter()
            .filter(|p| {
                matches!(
                    p.name.as_str(),
                    ATTR_RADIUS | ATTR_HEIGHT | ATTR_AXIS | ATTR_EXTENT
                )
            })
            .collect();

        if props.is_empty() {
            ui.label(
                egui::RichText::new("No cone attributes authored")
                    .small()
                    .weak(),
            );
            return;
        }

        let editable = ctx.is_prim_editable();
        let can_edit_radius = editable && claims.is_property_editable(ATTR_RADIUS);
        let can_edit_height = editable && claims.is_property_editable(ATTR_HEIGHT);
        let can_edit_axis = editable && claims.is_property_editable(ATTR_AXIS);

        for prop in props {
            match prop.name.as_str() {
                ATTR_RADIUS if can_edit_radius => {
                    super::draw_f64_attr(ui, ctx, prop, ATTR_RADIUS, DEFAULT_RADIUS, 0.01);
                }
                ATTR_HEIGHT if can_edit_height => {
                    super::draw_f64_attr(ui, ctx, prop, ATTR_HEIGHT, DEFAULT_HEIGHT, 0.01);
                }
                ATTR_AXIS if can_edit_axis => {
                    super::draw_axis_attr(ui, ctx, prop);
                }
                _ => {
                    attribute_row(ui, ctx, prop);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspector::PropertyClaimSet;

    #[test]
    fn claims_radius_height_axis_editable() {
        let mut claims = PropertyClaimSet::default();
        ConePanel.claim(&mut claims);
        for attr in [ATTR_RADIUS, ATTR_HEIGHT, ATTR_AXIS] {
            assert!(claims.is_claimed(attr), "{attr} must be claimed");
            assert!(claims.is_property_editable(attr), "{attr} must be editable");
        }
    }

    #[test]
    fn claims_extent_read_only() {
        let mut claims = PropertyClaimSet::default();
        ConePanel.claim(&mut claims);
        assert!(claims.is_claimed(ATTR_EXTENT));
        assert!(!claims.is_property_editable(ATTR_EXTENT));
    }

    #[test]
    fn does_not_claim_size() {
        let mut claims = PropertyClaimSet::default();
        ConePanel.claim(&mut claims);
        assert!(!claims.is_claimed("size"));
    }
}
