use egui::Ui;

use crate::inspector::{
    InspectorContext, InspectorContribution, PropertyClaimSet,
    widgets::{attribute_row, attribute_row_with_colorpicker},
};

const DISPLAY_COLOR: &str = "primvars:displayColor";
const DISPLAY_OPACITY: &str = "primvars:displayOpacity";

const MESH_ATTRS: &[&str] = &[
    "points",
    "normals",
    "faceVertexIndices",
    "faceVertexCounts",
    "primvars:st",
    DISPLAY_OPACITY,
    "doubleSided",
    "subdivisionScheme",
    "interpolateBoundary",
];

const EDITABLE_MESH_ATTRS: &[&str] = &[DISPLAY_COLOR];

/// "Mesh" section: vertex/face stats for `UsdGeomMesh` prims.
#[derive(Debug)]
pub struct MeshPanel;

impl InspectorContribution for MeshPanel {
    fn label(&self) -> &'static str {
        "Mesh"
    }
    fn default_open(&self) -> bool {
        false
    }
    fn priority(&self) -> i32 {
        0
    }
    fn claim(&self, claims: &mut PropertyClaimSet) {
        for &name in EDITABLE_MESH_ATTRS {
            claims.claim_editable(name);
        }
        for &name in MESH_ATTRS {
            claims.claim(name);
        }
    }

    fn present(&self, ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        ctx.type_name() == "Mesh"
            || ctx.type_name() == "Cube"
            || ctx.type_name() == "Cylinder"
            || ctx.type_name() == "Cone"
            || ctx.type_name() == "Sphere"
            || ctx.type_name() == "Capsule"
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, claims: &PropertyClaimSet) {
        let mesh_props: Vec<_> = ctx
            .properties
            .iter()
            .filter(|p| {
                MESH_ATTRS.contains(&p.name.as_str())
                    || EDITABLE_MESH_ATTRS.contains(&p.name.as_str())
            })
            .collect();

        if mesh_props.is_empty() {
            ui.label(
                egui::RichText::new("No geometry attributes authored")
                    .small()
                    .weak(),
            );
            return;
        }

        for prop in mesh_props {
            if prop.name.as_str() == DISPLAY_COLOR {
                let editable = ctx.is_prim_editable() && claims.is_property_editable(DISPLAY_COLOR);

                attribute_row_with_colorpicker(ui, ctx, prop, editable);
            } else {
                attribute_row(ui, ctx, prop);
            }
        }
    }
}

// ── Value summary parsing ─────────────────────────────────────────────────────
/*
/// Try to parse a `primvars:displayColor` `value_summary` string into an RGB
/// triple.
///
/// USD may prefix array value summaries with `"[N values] "` before the
/// `(r, g, b)` tuple, so the full string can look like:
/// - `"(0.9, 0.3, 0.2)"`
/// - `"[1 values] (0.9, 0.3, 0.2)"`
///
/// We find the innermost `(…)` pair and parse whatever is inside it.
/// Returns `None` when the format is unrecognised.
fn parse_color_summary(s: &str) -> Option<[f32; 3]> {
    let start = s.find('(')? + 1;
    let end = start + s[start..].find(')')?;
    let inner = &s[start..end];
    let mut parts = inner.splitn(3, ',');
    let r = parts.next()?.trim().parse::<f32>().ok()?;
    let g = parts.next()?.trim().parse::<f32>().ok()?;
    let b = parts.next()?.trim().parse::<f32>().ok()?;
    Some([r, g, b])
}

#[cfg(test)]
mod tests {
    use super::parse_color_summary;

    #[test]
    fn test_parse_color_paren_format() {
        let rgb = parse_color_summary("(0.18, 0.18, 0.18)").unwrap();
        assert!((rgb[0] - 0.18_f32).abs() < 1e-4);
        assert!((rgb[1] - 0.18_f32).abs() < 1e-4);
        assert!((rgb[2] - 0.18_f32).abs() < 1e-4);
    }

    #[test]
    fn test_parse_color_bracket_paren_format() {
        let rgb = parse_color_summary("[(1.0, 0.0, 0.5)]").unwrap();
        assert!((rgb[0] - 1.0_f32).abs() < 1e-4);
        assert!((rgb[1] - 0.0_f32).abs() < 1e-4);
        assert!((rgb[2] - 0.5_f32).abs() < 1e-4);
    }

    /// USD C++ shim formats VtVec3fArray value_summary as "[N values] (r, g, b)"
    #[test]
    fn test_parse_color_usd_array_prefix_format() {
        let rgb = parse_color_summary("[1 values] (0.9, 0.3, 0.2)").unwrap();
        assert!((rgb[0] - 0.9_f32).abs() < 1e-4);
        assert!((rgb[1] - 0.3_f32).abs() < 1e-4);
        assert!((rgb[2] - 0.2_f32).abs() < 1e-4);
    }

    #[test]
    fn test_parse_color_returns_none_for_garbage() {
        assert!(parse_color_summary("not a color").is_none());
        assert!(parse_color_summary("").is_none());
    }
}
*/
