//! Schema-free action declarations and the shared runtime actor transport.
use crate::inspector::{InspectorContext, InspectorContribution, PropertyClaimSet};
use idtx_editor_model::AnimationTransport;
use idtx_usd::AnimationClip;

#[derive(Clone, Debug)]
pub struct ClipView {
    pub label: String,
    pub declaration: Option<AnimationClip>,
    pub current: f64,
    pub playing: bool,
    pub independent: bool,
    pub completed: bool,
    pub error: String,
    pub last_command_error: String,
    pub effective_source: String,
}

#[derive(Clone, Debug, Default)]
pub struct AnimationInspector {
    pub clips: Vec<ClipView>,
    pub candidates: Vec<String>,
}

pub fn set_snapshot(ctx: &egui::Context, owner: &str, data: AnimationInspector) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(("animation-inspector", owner)), data));
}

#[derive(Debug)]
pub struct AnimationPanel;
impl InspectorContribution for AnimationPanel {
    fn label(&self) -> &'static str { "Animation clips" }
    fn default_open(&self) -> bool { true }
    fn priority(&self) -> i32 { -45 }
    fn claim(&self, _claims: &mut PropertyClaimSet) {}
    fn present(&self, ctx: &InspectorContext<'_>, _: &PropertyClaimSet) -> bool {
        matches!(ctx.type_name(), "Xform" | "SkelRoot") || ctx.properties.iter().any(|p| p.name == idtx_usd::ANIMATION_VERSION)
    }
    fn draw(&self, ui: &mut egui::Ui, ctx: &mut InspectorContext<'_>, _: &PropertyClaimSet) {
        let owner = ctx.info.path.clone();
        let snapshot = ui.ctx().data(|d| d.get_temp::<AnimationInspector>(egui::Id::new(("animation-inspector", &owner))))
            .unwrap_or_default();
        let editable = ctx.is_cookable();
        for entry in &snapshot.clips {
            ui.push_id(&entry.label, |ui| {
                egui::CollapsingHeader::new(&entry.label).default_open(true).show(ui, |ui| {
                    ui.label(if !entry.error.is_empty() { "Invalid" } else if entry.completed { "Completed" }
                        else if entry.playing { "Playing" } else if entry.independent { "Held" } else { "Following stage" });
                    if !entry.error.is_empty() { ui.colored_label(crate::theme::current().status_err, &entry.error); }
                    if !entry.last_command_error.is_empty() {
                        ui.colored_label(crate::theme::current().status_warn, format!("Command rejected: {}", entry.last_command_error));
                    }
                    ui.horizontal(|ui| {
                        for (text, action) in [("Play", AnimationTransport::Play), ("Pause", AnimationTransport::Pause),
                            ("Stop", AnimationTransport::Stop), ("Release", AnimationTransport::Release)] {
                            if ui.add_enabled(editable, egui::Button::new(text)).clicked() {
                                if let Some(sink) = ctx.sink.as_deref_mut() { sink.preview_animation_clip(&owner, &entry.label, action); }
                            }
                        }
                    });
                    if let Some(declaration) = &entry.declaration {
                        ui.horizontal(|ui| {
                            ui.label("Declared source:");
                            if declaration.source.is_empty() {
                                ui.label("composed attributes");
                            } else {
                                crate::inspector::widgets::prim_target_link(ui, ctx, &declaration.source);
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Effective source:");
                            if entry.effective_source.starts_with('/') {
                                crate::inspector::widgets::prim_target_link(ui, ctx, &entry.effective_source);
                            } else {
                                ui.label(&entry.effective_source);
                            }
                        });
                        ui.label("Authored targets:");
                        for target in &declaration.targets {
                            crate::inspector::widgets::prim_target_link(ui, ctx, target);
                        }
                        ui.label(format!("Stage range: {} … {}", declaration.start, declaration.end));
                        let mut current = entry.current;
                        if declaration.end > declaration.start && ui.add_enabled(editable,
                            egui::Slider::new(&mut current, declaration.start..=declaration.end).text("Time code")).changed() {
                            if let Some(sink) = ctx.sink.as_deref_mut() { sink.preview_animation_clip(&owner, &entry.label, AnimationTransport::Seek(current)); }
                        }
                        let draft_id = egui::Id::new(("animation-draft", &owner, &entry.label));
                        let mut draft = ui.ctx().data(|d| d.get_temp::<AnimationClip>(draft_id)).unwrap_or_else(|| declaration.clone());
                        let source_id = draft_id.with("authored");
                        let previous = ui.ctx().data(|d| d.get_temp::<AnimationClip>(source_id));
                        if previous.as_ref() != Some(declaration) { draft = declaration.clone(); }
                        ui.add_enabled_ui(editable, |ui| edit_fields(ui, &mut draft, &snapshot.candidates));
                        ui.horizontal(|ui| {
                            if ui.add_enabled(editable, egui::Button::new("Apply clip")).clicked() {
                                if let Some(sink) = ctx.sink.as_deref_mut() { sink.edit_animation_clip(draft.clone(), false); }
                            }
                            if ui.button("Reload fields").clicked() { draft = declaration.clone(); }
                            if ui.add_enabled(editable, egui::Button::new("Remove clip")).clicked() {
                                if let Some(sink) = ctx.sink.as_deref_mut() { sink.edit_animation_clip(declaration.clone(), true); }
                            }
                        });
                        ui.ctx().data_mut(|d| d.insert_temp(draft_id, draft));
                        ui.ctx().data_mut(|d| d.insert_temp(source_id, declaration.clone()));
                    }
                });
            });
        }
        if editable {
            let id = egui::Id::new(("animation-new-name", &owner));
            let mut name = ui.ctx().data(|d| d.get_temp::<String>(id)).unwrap_or_else(|| "idle".into());
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut name);
                if ui.button("Add clip").clicked() {
                    let clip = AnimationClip { owner: owner.clone(), name: name.clone(), source: String::new(),
                        targets: vec![owner.clone()], start: 0.0, end: 24.0, speed: 1.0, looping: false,
                        autoplay: false, channel: "body".into(), error: String::new() };
                    if let Some(sink) = ctx.sink.as_deref_mut() { sink.edit_animation_clip(clip, false); }
                }
            });
            ui.ctx().data_mut(|d| d.insert_temp(id, name));
        }
    }
}

fn edit_fields(ui: &mut egui::Ui, clip: &mut AnimationClip, candidates: &[String]) {
    ui.label("Target roots (one composed path per line)");
    let mut targets = clip.targets.join("\n");
    if ui.add(egui::TextEdit::multiline(&mut targets).desired_rows(2)).changed() {
        clip.targets = targets.lines().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect();
    }
    egui::ComboBox::from_id_salt("target-picker").selected_text("Add target…").show_ui(ui, |ui| {
        for path in candidates { if ui.selectable_label(false, path).clicked() && !clip.targets.contains(path) { clip.targets.push(path.clone()); } }
    });
    ui.horizontal(|ui| { ui.label("Source"); ui.text_edit_singleline(&mut clip.source); });
    egui::ComboBox::from_id_salt("source-picker").selected_text("Choose source…").show_ui(ui, |ui| {
        if ui.selectable_label(clip.source.is_empty(), "Composed attributes").clicked() { clip.source.clear(); }
        for path in candidates { if ui.selectable_label(clip.source == *path, path).clicked() { clip.source.clone_from(path); } }
    });
    ui.horizontal(|ui| {
        ui.label("Start"); ui.add(egui::DragValue::new(&mut clip.start));
        ui.label("End"); ui.add(egui::DragValue::new(&mut clip.end));
    });
    ui.horizontal(|ui| { ui.label("Speed"); ui.add(egui::DragValue::new(&mut clip.speed).speed(0.05)); });
    ui.horizontal(|ui| { ui.checkbox(&mut clip.looping, "Loop"); ui.checkbox(&mut clip.autoplay, "Autoplay"); });
    ui.horizontal(|ui| { ui.label("Channel"); ui.text_edit_singleline(&mut clip.channel); });
}
