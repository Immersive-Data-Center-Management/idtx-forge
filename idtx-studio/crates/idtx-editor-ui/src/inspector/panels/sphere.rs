use egui::Ui;

use crate::inspector::{
    InspectorContext, InspectorContribution, PropertyClaimSet, widgets::attribute_row,
};

/// USD schema default for `UsdGeomSphere.radius`.
const DEFAULT_RADIUS: f64 = 1.0;

const ATTR_RADIUS: &str = "radius";
const ATTR_EXTENT: &str = "extent";

/// "Sphere" section — unique `UsdGeomSphere` attribute (`radius`).
///
/// `radius` is applied uniformly in all three axes.  Common `UsdGeomGprim`
/// attributes (`doubleSided`, `primvars:displayColor`, etc.) are rendered by
/// [`MeshPanel`].
#[derive(Debug)]
pub struct SpherePanel;

impl InspectorContribution for SpherePanel {
    fn label(&self) -> &'static str {
        "Sphere"
    }
    fn priority(&self) -> i32 {
        5
    }

    fn claim(&self, claims: &mut PropertyClaimSet) {
        claims.claim_editable(ATTR_RADIUS);
        claims.claim(ATTR_EXTENT);
    }

    fn present(&self, ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        ctx.type_name() == "Sphere"
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, claims: &PropertyClaimSet) {
        let props: Vec<_> = ctx
            .properties
            .iter()
            .filter(|p| matches!(p.name.as_str(), ATTR_RADIUS | ATTR_EXTENT))
            .collect();

        if props.is_empty() {
            ui.label(
                egui::RichText::new("No sphere attributes authored")
                    .small()
                    .weak(),
            );
            return;
        }

        let editable = ctx.is_prim_editable() && claims.is_property_editable(ATTR_RADIUS);

        for prop in props {
            if prop.name.as_str() == ATTR_RADIUS && editable {
                super::draw_f64_attr(ui, ctx, prop, ATTR_RADIUS, DEFAULT_RADIUS, 0.01);
            } else {
                attribute_row(ui, ctx, prop);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspector::PropertyClaimSet;

    #[test]
    fn claims_radius_editable() {
        let mut claims = PropertyClaimSet::default();
        SpherePanel.claim(&mut claims);
        assert!(claims.is_claimed(ATTR_RADIUS));
        assert!(claims.is_property_editable(ATTR_RADIUS));
    }

    #[test]
    fn claims_extent_read_only() {
        let mut claims = PropertyClaimSet::default();
        SpherePanel.claim(&mut claims);
        assert!(claims.is_claimed(ATTR_EXTENT));
        assert!(!claims.is_property_editable(ATTR_EXTENT));
    }

    #[test]
    fn does_not_claim_size_or_height() {
        let mut claims = PropertyClaimSet::default();
        SpherePanel.claim(&mut claims);
        assert!(!claims.is_claimed("size"));
        assert!(!claims.is_claimed("height"));
        assert!(!claims.is_claimed("axis"));
    }
}
