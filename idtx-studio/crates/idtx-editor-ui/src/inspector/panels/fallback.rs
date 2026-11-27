use egui::Ui;

use crate::inspector::{
    InspectorContext, InspectorContribution, PropertyClaimSet,
    widgets::{attribute_row, attribute_row_with_colorpicker},
};

/// "Attributes" fallback section: all authored properties not claimed by any
/// other panel.
///
/// Always the last panel (priority = `i32::MAX`).  Because every specific panel
/// claims its properties before this runs, only truly unclaimed attributes
/// appear here.  Nothing is silently dropped.
#[derive(Debug)]
pub struct FallbackPanel;

impl InspectorContribution for FallbackPanel {
    fn label(&self) -> &'static str {
        "Attributes"
    }
    fn default_open(&self) -> bool {
        false
    }
    fn priority(&self) -> i32 {
        i32::MAX
    }
    fn claim(&self, _claims: &mut PropertyClaimSet) {}

    fn present(&self, ctx: &InspectorContext<'_>, claims: &PropertyClaimSet) -> bool {
        !claims.unclaimed_authored(ctx.properties).is_empty()
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, claims: &PropertyClaimSet) {
        let unclaimed = claims.unclaimed_authored(ctx.properties);
        if unclaimed.is_empty() {
            return;
        }

        for prop in unclaimed {
            if prop.type_name.eq_ignore_ascii_case("color3f") {
                attribute_row_with_colorpicker(ui, ctx, prop, false);
            } else {
                attribute_row(ui, ctx, prop);
            }
        }
    }
}
