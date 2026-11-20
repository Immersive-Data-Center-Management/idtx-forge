use egui::Ui;

use crate::inspector::{
    InspectorContext, InspectorContribution, PropertyClaimSet,
    widgets::{attribute_row, attribute_row_with_colorpicker},
};

/// Attribute name prefixes / names used by `UsdLux` light schemas.
const LIGHT_PREFIXES: &[&str] = &["inputs:"];
const LIGHT_ATTRS: &[&str] = &["light:shaderId", "texture:format"];
const EDITABLE_ATTRS: &[&str] = &[
    "inputs:angle",
    "inputs:color",
    //"inputs:colorTemperature",
    "inputs:exposure",
    "inputs:intensity",
];

fn is_light_type(type_name: &str) -> bool {
    type_name.ends_with("Light")
}

/// What the light inspector's shadow section should show for the selected prim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShadowSection {
    /// Not a Distant light — no shadow section at all (v1 casts Distant only).
    Hidden,
    /// Distant light without `ShadowAPI`, prim editable → offer the button.
    OfferAdd,
    /// `ShadowAPI` already applied → show the enabled note.
    Enabled,
    /// Distant light but not editable (read-only product / display-only prim).
    NotEditable,
}

/// Decide the shadow section state from the three gating inputs.
///
/// Distant-only (only Distant lights allocate shadow slots in v1); the button
/// appears only when `ShadowAPI` is absent *and* the prim is editable, so the
/// `ApplyAPI`/`RemoveAPI` pair is always symmetric.
fn shadow_section(is_distant: bool, has_shadow_api: bool, is_editable: bool) -> ShadowSection {
    if !is_distant {
        ShadowSection::Hidden
    } else if has_shadow_api {
        ShadowSection::Enabled
    } else if is_editable {
        ShadowSection::OfferAdd
    } else {
        ShadowSection::NotEditable
    }
}

/// "Light" section: `UsdLux` light schema attributes (all `inputs:*`).
#[derive(Debug)]
pub struct LightPanel;

impl InspectorContribution for LightPanel {
    fn label(&self) -> &'static str {
        "Light"
    }
    fn priority(&self) -> i32 {
        0
    }
    fn claim(&self, claims: &mut PropertyClaimSet) {
        for &name in EDITABLE_ATTRS {
            claims.claim_editable(name);
        }
        for prefix in LIGHT_PREFIXES {
            claims.claim_prefix(prefix);
        }
        for &name in LIGHT_ATTRS {
            claims.claim(name);
        }
    }

    fn present(&self, ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        is_light_type(ctx.type_name()) || ctx.has_applied_schema("LightAPI")
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, claims: &PropertyClaimSet) {
        // Capture the shadow-casting decision inputs *before* the property-grid
        // closure borrows `ctx.properties`: the "Add Shadow Casting" click calls
        // `ctx.apply_api_schema` (a `&mut ctx` borrow), which must not overlap
        // the grid's immutable `ctx.properties` borrow.
        let has_shadow_api = ctx.has_applied_schema("ShadowAPI");
        let is_editable = ctx.is_prim_editable();
        // Shadow casting is a Distant-light-only affordance in v1 (only Distant
        // lights allocate shadow slots; see the shadow-rendering plan).
        let is_distant = ctx.type_name().contains("Distant");

        let light_props: Vec<_> = ctx
            .properties
            .iter()
            .filter(|p| {
                LIGHT_PREFIXES
                    .iter()
                    .any(|prefix| p.name.starts_with(prefix))
                    || LIGHT_ATTRS.contains(&p.name.as_str())
            })
            .collect();

        if light_props.is_empty() {
            ui.label(
                egui::RichText::new("No light attributes authored")
                    .small()
                    .weak(),
            );
        } else {
            for prop in light_props {
                let editable =
                    ctx.is_prim_editable() && claims.is_property_editable(prop.name.as_str());

                if prop.type_name.as_str() == "color3f" {
                    attribute_row_with_colorpicker(ui, ctx, prop, editable);
                } else if editable {
                    super::draw_f32_attr(ui, ctx, prop, prop.name.as_str(), 0.0, 0.1);
                } else {
                    attribute_row(ui, ctx, prop);
                }
            }
        }

        // ── Shadow casting ──────────────────────────────────────────────────
        // The renderer treats `HasAPI<UsdLuxShadowAPI>` as "this light casts",
        // so applying the schema is the whole gesture (no extra `shadow:enable`
        // write). The button is Distant-only, editable-only, and shown only when
        // the schema is absent — so the reverse (RemoveAPI) is always symmetric.
        let mut clicked_add = false;
        match shadow_section(is_distant, has_shadow_api, is_editable) {
            ShadowSection::Hidden => {}
            ShadowSection::Enabled => {
                ui.separator();
                ui.label(
                    egui::RichText::new("● Shadow casting enabled (ShadowAPI)")
                        .small()
                        .weak(),
                );
            }
            ShadowSection::OfferAdd => {
                ui.separator();
                clicked_add = ui.button("Add Shadow Casting").clicked();
            }
            ShadowSection::NotEditable => {
                ui.separator();
                ui.label(
                    egui::RichText::new("Shadow casting not enabled")
                        .small()
                        .weak(),
                );
            }
        }
        // Author outside the `ctx.properties` borrow above.
        if clicked_add {
            ctx.apply_api_schema("ShadowAPI");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ShadowSection, is_light_type, shadow_section};

    #[test]
    fn light_type_detection() {
        assert!(is_light_type("DistantLight"));
        assert!(is_light_type("SphereLight"));
        assert!(!is_light_type("Mesh"));
    }

    #[test]
    fn shadow_section_offers_button_only_for_editable_distant_without_api() {
        // The one state that shows the button.
        assert_eq!(shadow_section(true, false, true), ShadowSection::OfferAdd);
    }

    #[test]
    fn shadow_section_hidden_for_non_distant_lights() {
        // Non-Distant lights never show the section, regardless of the other flags.
        for &has in &[true, false] {
            for &editable in &[true, false] {
                assert_eq!(shadow_section(false, has, editable), ShadowSection::Hidden);
            }
        }
    }

    #[test]
    fn shadow_section_enabled_when_api_present() {
        // Once applied, the note shows even in a read-only product.
        assert_eq!(shadow_section(true, true, true), ShadowSection::Enabled);
        assert_eq!(shadow_section(true, true, false), ShadowSection::Enabled);
    }

    #[test]
    fn shadow_section_not_editable_hides_button() {
        // Distant + absent API but not editable → no button, just a note.
        assert_eq!(
            shadow_section(true, false, false),
            ShadowSection::NotEditable
        );
    }
}
