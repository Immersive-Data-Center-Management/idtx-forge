use egui::{RichText, Ui};

use crate::inspector::{InspectorContext, InspectorContribution, PropertyClaimSet};

/// "Packed Stage" section: payload load state for reference boundary prims.
#[derive(Debug)]
pub struct PackedStagePanel;

impl InspectorContribution for PackedStagePanel {
    fn label(&self) -> &'static str {
        "Packed Stage"
    }
    fn priority(&self) -> i32 {
        200
    }
    fn claim(&self, _claims: &mut PropertyClaimSet) {}

    fn present(&self, ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        ctx.info.is_packed_boundary
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, _claims: &PropertyClaimSet) {
        egui::Grid::new("packed_stage_panel")
            .num_columns(2)
            .spacing([8.0, 2.0])
            .show(ui, |ui| {
                ui.label(RichText::new("has payload").strong().small());
                ui.label(RichText::new(if ctx.info.has_payload { "✔" } else { "✗" }).small());
                ui.end_row();

                if ctx.info.has_payload {
                    ui.label(RichText::new("payload loaded").strong().small());
                    ui.label(
                        RichText::new(if ctx.info.payload_loaded {
                            "✔ loaded"
                        } else {
                            "○ unloaded"
                        })
                        .small()
                        .monospace(),
                    );
                    ui.end_row();
                }
            });
    }
}
