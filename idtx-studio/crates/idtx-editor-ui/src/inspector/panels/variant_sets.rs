use egui::{RichText, Ui};

use crate::inspector::{InspectorContext, InspectorContribution, PropertyClaimSet};

/// "Variant Sets" section: one `ComboBox` per variant set.
///
/// When `ctx.sink` is provided (mode ≥ `ReadOnlyWithVariants`) selecting a
/// variant calls `EditSink::set_variant_selection`, which authors the change
/// on the stage session layer.  The resulting `TfNotice` resync flows through
/// `StudioSession::tick()` and updates the ECS, scene tree, and render world
/// before the next frame.
#[derive(Debug)]
pub struct VariantSetsPanel;

impl InspectorContribution for VariantSetsPanel {
    fn label(&self) -> &'static str {
        "Variant Sets"
    }
    fn default_open(&self) -> bool {
        true
    }
    fn priority(&self) -> i32 {
        100
    }
    fn claim(&self, _claims: &mut PropertyClaimSet) {}

    fn present(&self, ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        !ctx.info.variant_sets.is_empty()
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, _claims: &PropertyClaimSet) {
        // Clone the variant sets data so we can borrow `ctx` mutably below
        // without holding an immutable reference through the loop body.
        let variant_sets = ctx.info.variant_sets.clone();

        egui::Grid::new("variant_sets_panel")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                for vs in &variant_sets {
                    ui.label(RichText::new(&vs.name).strong());

                    let current = if vs.selection.is_empty() {
                        "(none)".to_owned()
                    } else {
                        vs.selection.clone()
                    };

                    let mut changed_to: Option<String> = None;

                    egui::ComboBox::from_id_salt(format!("vs_{}", vs.name))
                        .selected_text(&current)
                        .width(160.0)
                        .show_ui(ui, |ui| {
                            for choice in &vs.choices {
                                let selected = choice == &vs.selection;
                                if ui.selectable_label(selected, choice).clicked() && !selected {
                                    changed_to = Some(choice.clone());
                                }
                            }
                        });

                    if let Some(new_selection) = changed_to {
                        let set_name = vs.name.clone();
                        let before_selection = vs.selection.clone();
                        ctx.with_variant_selection(|path, sink| {
                            sink.set_variant_selection(
                                path,
                                &set_name,
                                &before_selection,
                                &new_selection,
                            );
                        });
                    }

                    ui.end_row();
                }
            });
    }
}
