use crate::model::ShellAction;
use idtx_studio_session::PointInstancerToolState;
use idtx_usd::point_instancer::{
    MAX_POINT_INSTANCES, MAX_POINT_PROTOTYPES, PointInstancerPlacement,
};

#[derive(Clone)]
struct Draft {
    revision: u64,
    placement: PointInstancerPlacement,
}

pub(crate) fn panel(
    ui: &mut egui::Ui,
    tab_id: u64,
    path: &str,
    state: &PointInstancerToolState,
    authoring: bool,
    actions: &mut Vec<ShellAction>,
) {
    let editable = authoring && !state.read_only && !state.inspection.truncated;
    let key = egui::Id::new(("native-point-instancer", tab_id, path));
    egui::CollapsingHeader::new("Native PointInstancer").default_open(true).show(ui, |ui| {
        let inspection = &state.inspection;
        let visible = if inspection.mask.is_empty() { inspection.count as usize }
            else { inspection.mask.iter().filter(|v| **v != 0).count() };
        ui.label(format!("{} placements · {visible} unmasked · {} prototypes", inspection.count, inspection.prototypes.len()));
        if state.read_only { ui.strong("Derived runtime values — read-only during play"); }
        ui.small("Local USD coordinates · unit XYZW quaternions · binary visibility by stable id");
        ui.small("Use an emissive prototype for glow. Instances have no individual prim/entity highlight.");
        let mut guides = state.guides;
        if ui.checkbox(&mut guides, "Show placement guides (prototype colours / hidden grey / invalid red)").changed() {
            actions.push(ShellAction::PointInstancerGuides { tab_id, enabled: guides });
        }
        ui.small("Non-finite placements are marked at the instancer origin.");
        egui::CollapsingHeader::new("Prototypes and masks").show(ui, |ui| {
            for (i,p) in inspection.prototypes.iter().enumerate() { ui.monospace(format!("{i}: {p}")); }
            ui.label(format!("invisibleIds: {:?}", inspection.placement.invisible_ids));
            ui.label(format!("inactiveIds (non-time-varying metadata): {:?}", inspection.inactive_ids));
        });
        egui::CollapsingHeader::new("Validation / geometry and memory cost").default_open(true).show(ui, |ui| {
            ui.label(format!("Runtime bounds: {MAX_POINT_INSTANCES} placements / {MAX_POINT_PROTOTYPES} prototypes"));
            let a = &inspection.placement;
            let wire_bytes = a.ids.len() * 56 + a.invisible_ids.len() * 8;
            ui.label(format!("Placement command: {:.1} KiB · mask-only: {:.1} KiB", wire_bytes as f64/1024.0, a.invisible_ids.len() as f64/128.0));
            ui.small("Host argument budget defaults to 64 KiB; large allocations need a larger authored behavior-work budget. Queue limits also apply.");
            if state.geometry_cost_available {
                ui.label(format!("{} prototype/material batches · {} visible triangles", state.draw_batches, state.visible_triangles));
                ui.label(format!("Prototype GPU geometry: {:.1} KiB · full-pool instance capacity: {:.1} KiB",
                    state.prototype_gpu_bytes as f64/1024.0, state.placement_gpu_bytes as f64/1024.0));
                ui.small("Estimates exclude textures, materials, USD storage and driver overhead. Each prototype mesh/subset adds an instance buffer.");
            } else { ui.label("Render the viewport to obtain geometry cost estimates."); }
            for message in &inspection.diagnostics { ui.colored_label(egui::Color32::LIGHT_RED, message); }
            if inspection.diagnostics.is_empty() && state.invalid_entries.is_empty() { ui.label("Array / id / prototype validation passed."); }
        });
        let mut page = ui.data(|d| d.get_temp::<usize>(key.with("page"))).unwrap_or(0);
        ui.horizontal(|ui| {
            ui.label("Entries (64 per page)");
            ui.add(egui::DragValue::new(&mut page).range(0..=inspection.placement.ids.len().saturating_sub(1)/64));
        });
        ui.data_mut(|d| d.insert_temp(key.with("page"), page));
        if editable {
            let mut draft = ui.data(|d| d.get_temp::<Draft>(key)).filter(|d| d.revision == state.revision)
                .unwrap_or_else(|| Draft { revision: state.revision, placement: inspection.placement.clone() });
            // Only optional arrays have schema fallbacks. Materialize them in the
            // draft, not USD, so edits still commit as one validated transaction.
            let n = draft.placement.ids.len();
            if draft.placement.orientations_xyzw.is_empty() { draft.placement.orientations_xyzw.resize(n, [0.0,0.0,0.0,1.0]); }
            if draft.placement.scales.is_empty() { draft.placement.scales.resize(n, [1.0;3]); }
            egui::ScrollArea::vertical().max_height(280.0).id_salt(key.with("entries")).show(ui, |ui| {
                for i in (page*64)..((page+1)*64).min(n) {
                    let a = &mut draft.placement;
                    let old_id = a.ids[i];
                    let mut hidden = a.invisible_ids.contains(&old_id);
                    ui.push_id(i, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(format!("#{i} id")); ui.add(egui::DragValue::new(&mut a.ids[i]));
                            if let Some(p) = a.prototype_indices.get_mut(i) {
                                ui.label("prototype"); ui.add(egui::DragValue::new(p).range(0..=inspection.prototypes.len().saturating_sub(1) as i32));
                            }
                            ui.checkbox(&mut hidden, "hidden");
                        });
                        for (label, values) in [("position", a.positions.get_mut(i).map(|v| &mut v[..])),
                            ("XYZW", a.orientations_xyzw.get_mut(i).map(|v| &mut v[..])), ("scale", a.scales.get_mut(i).map(|v| &mut v[..]))] {
                            if let Some(values) = values { ui.horizontal(|ui| { ui.label(label); for v in values { ui.add(egui::DragValue::new(v).speed(0.01)); } }); }
                        }
                        if let Some(error) = state.invalid_entries.get(&i) { ui.colored_label(egui::Color32::LIGHT_RED, error); }
                    });
                    a.invisible_ids.retain(|id| *id != old_id);
                    if hidden { a.invisible_ids.push(a.ids[i]); }
                }
            });
            #[cfg(feature = "authoring")]
            if ui.button("Apply authored defaults").clicked() {
                actions.push(ShellAction::EditPointInstancer { tab_id, path: path.into(), placement: draft.placement.clone() });
            }
            ui.small("Defaults use normal Save / Undo. Runtime session-layer pulses are not saved here.");
            ui.data_mut(|d| d.insert_temp(key, draft));
            #[cfg(feature = "authoring")]
            egui::CollapsingHeader::new("Use F2 baked placements").show(ui, |ui| {
                let mut choice = ui.data(|d| d.get_temp::<(String,i32)>(key.with("bake"))).unwrap_or_default();
                egui::ComboBox::from_id_salt(key.with("bakes")).selected_text(if choice.0.is_empty() { "Select F2 output" } else { &choice.0 }).show_ui(ui, |ui| {
                    for source in &state.baked_placements { ui.selectable_value(&mut choice.0, source.clone(), source); }
                });
                ui.horizontal(|ui| { ui.label("Prototype index"); ui.add(egui::DragValue::new(&mut choice.1).range(0..=inspection.prototypes.len().saturating_sub(1) as i32)); });
                if ui.add_enabled(!choice.0.is_empty(), egui::Button::new("Import baked placements as defaults")).clicked() {
                    actions.push(ShellAction::ImportPointInstancerBake { tab_id, path: path.into(), output: choice.0.clone(), prototype_index: choice.1 });
                }
                ui.small("Generate with F2: select a BasisCurves prim → Bake placements along curve. Import copies that bake; reimport explicitly after a rebake.");
                ui.data_mut(|d| d.insert_temp(key.with("bake"), choice));
            });
        } else {
            egui::ScrollArea::vertical().max_height(260.0).id_salt(key.with("readonly")).show(ui, |ui| {
                let a = &inspection.placement;
                for i in (page*64)..((page+1)*64).min(a.ids.len().max(a.positions.len())) {
                    ui.monospace(format!("#{i} id {:?} · proto {:?} · {}", a.ids.get(i), a.prototype_indices.get(i),
                        if inspection.mask.is_empty() || inspection.mask.get(i) == Some(&1) { "unmasked" } else { "masked" }));
                    ui.label(format!("p {:?} · q {:?} · s {:?}", a.positions.get(i), a.orientations_xyzw.get(i), a.scales.get(i)));
                    if let Some(error) = state.invalid_entries.get(&i) { ui.colored_label(egui::Color32::LIGHT_RED, error); }
                }
            });
        }
    });
}
