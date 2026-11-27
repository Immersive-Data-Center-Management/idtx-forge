use egui::Ui;

use crate::inspector::{
    InspectorContext, InspectorContribution, PropertyClaimSet, widgets::attribute_row,
};

/// USD schema default for `UsdGeomCube.size` (edge length in local space).
const DEFAULT_SIZE: f64 = 2.0;

const ATTR_SIZE: &str = "size";
const ATTR_EXTENT: &str = "extent";

/// "Cube" section — unique `UsdGeomCube` attribute (`size`).
///
/// `size` specifies the length of each edge of the cube in local space.
/// The `extent` attribute (auto-computed bounding box from `UsdGeomBoundable`)
/// is shown read-only.  Common `UsdGeomGprim` attributes (`doubleSided`,
/// `primvars:displayColor`, etc.) are rendered by [`MeshPanel`].
#[derive(Debug)]
pub struct CubePanel;

impl InspectorContribution for CubePanel {
    fn label(&self) -> &'static str {
        "Cube"
    }
    /// Appear after the shared [`MeshPanel`] section (priority 0).
    fn priority(&self) -> i32 {
        5
    }

    fn claim(&self, claims: &mut PropertyClaimSet) {
        claims.claim_editable(ATTR_SIZE);
        claims.claim(ATTR_EXTENT);
    }

    fn present(&self, ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        ctx.type_name() == "Cube"
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, claims: &PropertyClaimSet) {
        let props: Vec<_> = ctx
            .properties
            .iter()
            .filter(|p| matches!(p.name.as_str(), ATTR_SIZE | ATTR_EXTENT))
            .collect();

        if props.is_empty() {
            ui.label(
                egui::RichText::new("No cube attributes authored")
                    .small()
                    .weak(),
            );
            return;
        }

        let editable = ctx.is_prim_editable() && claims.is_property_editable(ATTR_SIZE);

        for prop in props {
            if prop.name.as_str() == ATTR_SIZE && editable {
                super::draw_f64_attr(ui, ctx, prop, ATTR_SIZE, DEFAULT_SIZE, 0.1);
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
    fn claims_size_editable() {
        let mut claims = PropertyClaimSet::default();
        CubePanel.claim(&mut claims);
        assert!(claims.is_claimed(ATTR_SIZE));
        assert!(claims.is_property_editable(ATTR_SIZE));
    }

    #[test]
    fn claims_extent_read_only() {
        let mut claims = PropertyClaimSet::default();
        CubePanel.claim(&mut claims);
        assert!(claims.is_claimed(ATTR_EXTENT));
        assert!(!claims.is_property_editable(ATTR_EXTENT));
    }

    #[test]
    fn does_not_claim_radius_or_height() {
        let mut claims = PropertyClaimSet::default();
        CubePanel.claim(&mut claims);
        assert!(!claims.is_claimed("radius"));
        assert!(!claims.is_claimed("height"));
        assert!(!claims.is_claimed("axis"));
    }
}
