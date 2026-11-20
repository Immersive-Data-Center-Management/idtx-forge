use egui::Ui;

use crate::inspector::{
    InspectorContext, InspectorContribution, PropertyClaimSet, widgets::attribute_row,
};

/// USD schema defaults for `UsdGeomPlane`.
const DEFAULT_WIDTH: f64 = 2.0;
const DEFAULT_LENGTH: f64 = 2.0;

const ATTR_WIDTH: &str = "width";
const ATTR_LENGTH: &str = "length";
const ATTR_AXIS: &str = "axis";
const ATTR_EXTENT: &str = "extent";

/// "Plane" section — unique `UsdGeomPlane` attributes.
///
/// | Attribute | USD type | Meaning |
/// |-----------|----------|---------|
/// | `axis`    | `token`  | Plane normal: `"X"`, `"Y"`, or `"Z"` (default). |
/// | `width`   | `double` | Extent on the first axis perpendicular to `axis`. |
/// | `length`  | `double` | Extent on the second axis perpendicular to `axis`. |
///
/// The width/length axis assignment depends on `axis` (X→Z/Y, Y→X/Z, Z→X/Y).
/// The `extent` attribute (auto-computed via `UsdGeomBoundable`) is read-only.
/// Common `UsdGeomGprim` attributes are rendered by [`MeshPanel`](super::super).
#[derive(Debug)]
pub struct PlanePanel;

impl InspectorContribution for PlanePanel {
    fn label(&self) -> &'static str {
        "Plane"
    }
    /// Appear after the shared [`MeshPanel`] section (priority 0).
    fn priority(&self) -> i32 {
        5
    }

    fn claim(&self, claims: &mut PropertyClaimSet) {
        claims.claim_editable(ATTR_WIDTH);
        claims.claim_editable(ATTR_LENGTH);
        claims.claim_editable(ATTR_AXIS);
        claims.claim(ATTR_EXTENT);
    }

    fn present(&self, ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        ctx.type_name() == "Plane"
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, claims: &PropertyClaimSet) {
        let props: Vec<_> = ctx
            .properties
            .iter()
            .filter(|p| {
                matches!(
                    p.name.as_str(),
                    ATTR_WIDTH | ATTR_LENGTH | ATTR_AXIS | ATTR_EXTENT
                )
            })
            .collect();

        if props.is_empty() {
            ui.label(
                egui::RichText::new("No plane attributes authored")
                    .small()
                    .weak(),
            );
            return;
        }

        let editable = ctx.is_prim_editable();
        let can_edit_width = editable && claims.is_property_editable(ATTR_WIDTH);
        let can_edit_length = editable && claims.is_property_editable(ATTR_LENGTH);
        let can_edit_axis = editable && claims.is_property_editable(ATTR_AXIS);

        for prop in props {
            match prop.name.as_str() {
                ATTR_WIDTH if can_edit_width => {
                    super::draw_f64_attr(ui, ctx, prop, ATTR_WIDTH, DEFAULT_WIDTH, 0.01);
                }
                ATTR_LENGTH if can_edit_length => {
                    super::draw_f64_attr(ui, ctx, prop, ATTR_LENGTH, DEFAULT_LENGTH, 0.01);
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
    fn claims_width_length_axis_editable() {
        let mut claims = PropertyClaimSet::default();
        PlanePanel.claim(&mut claims);
        for attr in [ATTR_WIDTH, ATTR_LENGTH, ATTR_AXIS] {
            assert!(claims.is_claimed(attr), "{attr} must be claimed");
            assert!(claims.is_property_editable(attr), "{attr} must be editable");
        }
    }

    #[test]
    fn claims_extent_read_only() {
        let mut claims = PropertyClaimSet::default();
        PlanePanel.claim(&mut claims);
        assert!(claims.is_claimed(ATTR_EXTENT));
        assert!(!claims.is_property_editable(ATTR_EXTENT));
    }

    #[test]
    fn does_not_claim_size_or_radius() {
        let mut claims = PropertyClaimSet::default();
        PlanePanel.claim(&mut claims);
        assert!(!claims.is_claimed("size"));
        assert!(!claims.is_claimed("radius"));
        assert!(!claims.is_claimed("height"));
    }
}
