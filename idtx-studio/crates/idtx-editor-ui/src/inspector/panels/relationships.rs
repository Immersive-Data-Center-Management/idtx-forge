use egui::Ui;
use idtx_usd::PropertyKind;

use crate::inspector::{
    InspectorContext, InspectorContribution, PropertyClaimSet, widgets::relationship_row,
};

/// "Relationships" section: all authored relationship properties.
#[derive(Debug)]
pub struct RelationshipsPanel;

impl InspectorContribution for RelationshipsPanel {
    fn label(&self) -> &'static str {
        "Relationships"
    }
    fn default_open(&self) -> bool {
        false
    }
    fn priority(&self) -> i32 {
        500
    }
    fn claim(&self, _claims: &mut PropertyClaimSet) {
        // TODO: how to claim relationships during panel registration that happens without the context of a Prim
        /*
        let rels: Vec<_> = ctx
            .properties
            .iter()
            .filter(|p| p.authored && matches!(p.kind, PropertyKind::Relationship))
            .collect();

        // Claim all relationship properties.
        for r in &rels {
            claims.claim(&r.name);
        }
        */
    }

    fn present(&self, ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        ctx.has_authored_relationships()
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, _claims: &PropertyClaimSet) {
        let rels: Vec<_> = ctx
            .properties
            .iter()
            .filter(|p| p.authored && matches!(p.kind, PropertyKind::Relationship))
            .collect();

        egui::Grid::new("relationships_panel")
            .num_columns(3)
            .spacing([8.0, 2.0])
            .show(ui, |ui| {
                for prop in rels {
                    relationship_row(ui, ctx, prop);
                }
            });
    }
}
