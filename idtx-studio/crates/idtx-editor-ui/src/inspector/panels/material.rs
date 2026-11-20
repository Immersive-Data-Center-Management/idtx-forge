use egui::Ui;

use crate::inspector::{InspectorContext, InspectorContribution, PropertyClaimSet};

/// "Material" inspector section.
///
/// Shows `primvars:displayColor[0]` as an interactive colour picker when the
/// prim is editable (Poser, root-layer opinion).  In read-only contexts it
/// falls back to the value-summary string from USD.  `primvars:displayOpacity`
/// is shown as a read-only float for now.
#[derive(Debug)]
pub struct MaterialPanel;

impl InspectorContribution for MaterialPanel {
    fn label(&self) -> &'static str {
        "Material"
    }
    fn default_open(&self) -> bool {
        false
    }
    fn priority(&self) -> i32 {
        -50
    }

    fn claim(&self, _claims: &mut PropertyClaimSet) {}

    fn present(&self, _ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        false
    }

    fn draw(&self, ui: &mut Ui, _ctx: &mut InspectorContext<'_>, _claims: &PropertyClaimSet) {
        egui::Grid::new("material_panel")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |_ui| {});
    }
}
