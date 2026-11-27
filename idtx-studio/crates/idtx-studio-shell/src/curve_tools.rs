use crate::model::ShellAction;
use idtx_usd::curve::{CurveSpace, placement::PlacementSettings};

pub(crate) fn clear_settings(ctx: &egui::Context, tab_id: u64, path: &str) {
    ctx.data_mut(|d| {
        d.remove::<PlacementSettings>(egui::Id::new(("curve-settings", tab_id, path)))
    });
}

pub(crate) fn panel(
    ui: &mut egui::Ui,
    tab_id: u64,
    info: &idtx_editor_model::PrimInfo,
    state: &idtx_studio_session::CurveToolState,
    editable: bool,
    conductor: bool,
    actions: &mut Vec<ShellAction>,
) {
    egui::CollapsingHeader::new("Native curve / placements").default_open(true).show(ui, |ui| {
        let source = &state.curve.source;
        ui.label(format!("{} / {} / {} · {} curves", source.curve_type, source.basis, source.wrap, source.counts.len()));
        ui.label(format!("Route eligible: {} · endpoint groups: '{}' → '{}'", source.eligible, source.start_group, source.end_group));
        let key = egui::Id::new(("curve-settings", tab_id, &info.path));
        let mut settings = ui.data(|d| d.get_temp::<PlacementSettings>(key)).unwrap_or_else(|| state.settings.clone());
        ui.horizontal(|ui| {
            ui.label("Curve index");
            if ui.add(egui::DragValue::new(&mut settings.curve_index).range(0..=source.counts.len().saturating_sub(1) as u32)).changed() {
                actions.push(ShellAction::SelectCurveIndex {tab_id,path:info.path.clone(),index:settings.curve_index});
            }
        });
        match state.curve.curve(settings.curve_index as usize) {
            Ok(curve) => {
                ui.label(format!("Length {:.6} m · tolerance {:.6} m", curve.length(), curve.tolerance()));
                for (name, distance) in [("Start (green)", 0.0), ("End (red)", curve.length())] {
                    if let Ok(s) = curve.sample_distance(distance, CurveSpace::StageWorldMetres) {
                        ui.label(format!("{name}: {:.3?}", s.point.to_array()));
                    }
                }
                ui.small("Arrow follows increasing distance. Coordinates: Y-up stage-world metres.");
            }
            Err(error) => { ui.colored_label(egui::Color32::LIGHT_RED, error.to_string()); }
        }
        #[cfg(feature = "authoring")]
        if editable && info.is_locally_authored && source.validate().is_ok() {
            egui::CollapsingHeader::new("Route eligibility / accuracy").show(ui, |ui| {
                let route_key = key.with(("route",source.tolerance.to_bits(),source.eligible,&source.start_group,&source.end_group));
                let mut route = ui.data(|d| d.get_temp::<(f64,bool,String,String)>(route_key))
                    .unwrap_or((source.tolerance, source.eligible, source.start_group.clone(), source.end_group.clone()));
                ui.horizontal(|ui| { ui.label("Tolerance (metres)"); ui.add(egui::DragValue::new(&mut route.0).speed(0.0001).range(1e-9..=1.0)); });
                ui.checkbox(&mut route.1, "Eligible route");
                ui.horizontal(|ui| {ui.label("Start group");ui.text_edit_singleline(&mut route.2);});
                ui.horizontal(|ui| {ui.label("End group");ui.text_edit_singleline(&mut route.3);});
                if ui.button("Apply route settings").clicked() {
                    actions.push(ShellAction::EditCurveRoute {tab_id,path:info.path.clone(),tolerance:route.0,eligible:route.1,start_group:route.2.clone(),end_group:route.3.clone()});
                }
                ui.data_mut(|d| d.insert_temp(route_key,route));
            });
            egui::CollapsingHeader::new("Control points / Bezier handles (local)").show(ui, |ui| {
                let points_key = key.with(("points", state.curve.geometric_revision));
                let mut points = ui.data(|d| d.get_temp::<Vec<[f32;3]>>(points_key)).unwrap_or_else(|| source.points.clone());
                let mut page = ui.data(|d| d.get_temp::<usize>(key.with("page"))).unwrap_or(0);
                ui.horizontal(|ui| { ui.label("Control page (64 vertices)"); ui.add(egui::DragValue::new(&mut page).range(0..=points.len().saturating_sub(1)/64)); });
                ui.data_mut(|d| d.insert_temp(key.with("page"), page));
                egui::ScrollArea::vertical().max_height(220.0).id_salt(key).show(ui, |ui| {
                    for (i, point) in points.iter_mut().enumerate().skip(page*64).take(64) {
                        ui.horizontal(|ui| {
                            let offset: usize = source.counts.iter().scan(0usize, |total, &n| { let old = *total; *total += n as usize; Some((old,*total)) }).find(|&(_,end)| i < end).map_or(0, |(start,_)| start);
                            let label = if source.curve_type == "cubic" && (i-offset)%3 != 0 { "handle" } else { "vertex" };
                            ui.label(format!("{i} {label}"));
                            for value in point { ui.add(egui::DragValue::new(value).speed(0.01)); }
                        });
                    }
                });
                let apply = ui.button("Apply control points").clicked();
                let reverse = ui.button("Reverse each curve").clicked();
                let added = if source.curve_type == "cubic" {3} else {1};
                let append = source.wrap == "nonperiodic" && ui.add_enabled(points.len()+added <= idtx_usd::curve::MAX_CURVE_POINTS,
                    egui::Button::new("Append segment to selected curve")).clicked();
                let mut counts = source.counts.clone();
                if reverse {
                    let mut offset = 0;
                    for &n in &source.counts {
                        let start = offset + usize::from(source.wrap == "periodic");
                        points[start..offset+n as usize].reverse(); offset += n as usize;
                    }
                }
                if append {
                    let index = settings.curve_index as usize;
                    let end: usize = counts[..=index].iter().map(|&n| n as usize).sum();
                    let last = glam::Vec3::from_array(points[end-1]);
                    let direction = (last-glam::Vec3::from_array(points[end-2])).try_normalize().unwrap_or(glam::Vec3::X);
                    let n = if source.curve_type == "cubic" {3} else {1};
                    points.splice(end..end,(1..=n).map(|i|(last+direction*i as f32).to_array()));
                    counts[index] += n;
                }
                if apply || reverse || append {
                    actions.push(ShellAction::EditCurvePoints { tab_id, path: info.path.clone(), points: points.clone(), counts });
                }
                ui.data_mut(|d| d.insert_temp(points_key, points));
            });
        }
        if conductor && editable {
            ui.separator();
            for (name, value) in [("Spacing (m)", &mut settings.spacing), ("Reveal start (m)", &mut settings.start), ("Reveal window (m)", &mut settings.window)] {
                ui.horizontal(|ui| { ui.label(name); ui.add(egui::DragValue::new(value).speed(0.01).range(0.0..=1e6)); });
            }
            for (label, values) in [("Alternate lateral offsets (m)", &mut settings.lateral[..]), ("Floor normal", &mut settings.floor_normal[..]), ("Full-object clearance half extents (m)", &mut settings.half_extents[..])] {
                ui.label(label); ui.horizontal(|ui| { for v in values { ui.add(egui::DragValue::new(v).speed(0.01)); } });
            }
            ui.checkbox(&mut settings.orient, "Orient +Z along floor tangent");
            ui.horizontal(|ui| { ui.label("Deterministic seed (offset phase)"); ui.add(egui::DragValue::new(&mut settings.seed)); });
            if ui.button("Preview spacing and loaded-scene clearance").clicked() {
                actions.push(ShellAction::PreviewCurve { tab_id, path: info.path.clone(), settings: settings.clone() });
            }
            #[cfg(feature = "authoring")]
            if ui.button("Bake placements along curve").clicked() {
                actions.push(ShellAction::BakeCurve { tab_id, path: info.path.clone(), settings: settings.clone() });
            }
            ui.small("Bake saves settings and native Xforms to a tool-owned Scope. Maximum 4096 placements. Preview/bake samples default USD time.");
        }
        ui.label(&state.message);
        ui.data_mut(|d| d.insert_temp(key, settings));
    });
}
