use egui::Ui;

use crate::inspector::{
    InspectorContext, InspectorContribution, PropertyClaimSet, widgets::label_value_row,
};

const COMMON_PROPS: &[&str] = &["visibility", "purpose", "active"];

/// "Common" section: visibility, purpose, active flag.
#[derive(Debug)]
pub struct CommonPanel;

impl InspectorContribution for CommonPanel {
    fn label(&self) -> &'static str {
        "Common"
    }
    fn collapsible(&self) -> bool {
        false
    }
    fn priority(&self) -> i32 {
        -900
    }

    fn claim(&self, claims: &mut PropertyClaimSet) {
        // Claim these regardless of whether authored, so they don't appear
        // in the fallback panel.
        for &name in COMMON_PROPS {
            claims.claim(name);
        }
    }

    fn present(&self, _ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        true
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, _claims: &PropertyClaimSet) {
        egui::Grid::new("common_panel")
            .num_columns(2)
            .spacing([8.0, 2.0])
            .show(ui, |ui| {
                // Visibility: show the live ECS-derived state + authored opinion
                {
                    ui.label(egui::RichText::new("visibility").strong());
                    ui.horizontal(|ui| {
                        if let Some(prop) = ctx.property("visibility") {
                            let authored = &prop.value_summary;
                            ui.label(
                                egui::RichText::new(authored.as_str())
                                    .small()
                                    .weak()
                                    .monospace(),
                            );
                        }
                    });
                    ui.end_row();
                }

                // Purpose
                if let Some(prop) = ctx.property("purpose") {
                    label_value_row(ui, "purpose", &prop.value_summary.clone());
                }

                // Active
                if let Some(prop) = ctx.property("active") {
                    label_value_row(ui, "active", &prop.value_summary.clone());
                }
            });
    }
}
