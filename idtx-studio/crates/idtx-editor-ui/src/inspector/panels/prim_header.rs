use egui::{RichText, Ui};

use crate::{
    icons,
    inspector::{InspectorContext, InspectorContribution, PropertyClaimSet},
    scene_tree::classify_schema_icon,
};

/// Always-first, non-collapsible header: type icon + type-name chip + full path.
#[derive(Debug)]
pub struct PrimHeaderPanel;

impl InspectorContribution for PrimHeaderPanel {
    fn label(&self) -> &'static str {
        "Prim"
    }
    fn collapsible(&self) -> bool {
        false
    }
    fn priority(&self) -> i32 {
        -1000
    }
    fn claim(&self, _claims: &mut PropertyClaimSet) {}

    fn present(&self, _ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        true
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, _claims: &PropertyClaimSet) {
        let info = ctx.info;

        // Row 1: type icon + type name chip
        ui.horizontal(|ui| {
            let type_icon = classify_schema_icon(&info.type_name);
            ui.add(
                egui::Image::new(type_icon)
                    .fit_to_exact_size(egui::Vec2::splat(16.0))
                    .tint(ui.visuals().text_color()),
            );

            let type_label = if info.type_name.is_empty() {
                "Unknown".to_owned()
            } else {
                info.type_name.clone()
            };
            ui.label(
                RichText::new(&type_label)
                    .strong()
                    .small()
                    .background_color(ui.visuals().selection.bg_fill.linear_multiply(0.35)),
            );

            if info.is_locally_authored {
                ui.label(RichText::new("✎ local").small().weak());
            }
            if info.is_packed_boundary {
                ui.add(
                    egui::Image::new(icons::PACKED)
                        .fit_to_exact_size(egui::Vec2::splat(14.0))
                        .tint(ui.visuals().weak_text_color()),
                );
            }
        });

        // Row 2: full path (selectable for copy)
        let last_segment = info.path.rsplit('/').next().unwrap_or(info.path.as_str());
        ui.horizontal(|ui| {
            ui.label(RichText::new(last_segment).strong().monospace());
        });
        ui.label(RichText::new(&info.path).small().weak().monospace())
            .on_hover_text("Full SdfPath — click to select all");

        // Row 3: applied-schema chips (if any)
        if !info.applied_schemas.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Applied APIs:").small().weak());
                for schema in &info.applied_schemas {
                    let short = schema.strip_suffix("API").unwrap_or(schema.as_str());
                    ui.label(
                        RichText::new(short)
                            .small()
                            .monospace()
                            .background_color(ui.visuals().code_bg_color),
                    );
                }
            });
        }

        // Row 4: kind (if set)
        if !info.kind.is_empty() {
            ui.label(RichText::new(format!("kind: {}", info.kind)).small().weak());
        }

        ui.separator();
    }
}
