use std::collections::HashMap;

use egui::{Frame, Label, Margin, RichText, Ui};
use idtx_usd::SkeletonJointRecord;
use num_traits::ToPrimitive;

use crate::icons;
use crate::inspector::{InspectorContext, InspectorContribution, PropertyClaimSet};

const SKEL_ATTRS: &[&str] = &[
    "joints",
    "bindTransforms",
    "restTransforms",
    "animationSource",
];

const TREE_MAX_HEIGHT: f32 = 300.0;
const ICON_SIZE: f32 = 12.0;
const ROW_HEIGHT: f32 = ICON_SIZE + 4.0;
const INDENT_PX: f32 = 8.0;

/// Inspector panel for `Skeleton` and `SkelRoot` prims.
///
/// Shows the skeleton's joint hierarchy as a collapsible tree inside a
/// dark scrollable container.  Joint data is read from
/// `InspectorContext::skeleton_joints`.
#[derive(Debug)]
pub struct SkeletonPanel;

impl InspectorContribution for SkeletonPanel {
    fn label(&self) -> &'static str {
        "Skeleton Joints"
    }
    fn default_open(&self) -> bool {
        true
    }
    fn priority(&self) -> i32 {
        -50
    }

    fn claim(&self, claims: &mut PropertyClaimSet) {
        for &name in SKEL_ATTRS {
            claims.claim(name);
        }
    }

    fn present(&self, ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        ctx.type_name() == "Skeleton"
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, _claims: &PropertyClaimSet) {
        let Some(joints) = ctx.skeleton_joints else {
            ui.label(RichText::new("No skeleton topology loaded").small().weak());
            return;
        };

        if joints.is_empty() {
            ui.label(RichText::new("Skeleton has no joints").small().weak());
            return;
        }

        // Build children map: parent_index → [child_indices].
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut roots: Vec<u32> = Vec::new();
        for (i, joint) in joints.iter().enumerate() {
            match joint.parent {
                None => roots.push(i.to_u32().unwrap_or(u32::MAX)),
                Some(parent) => children
                    .entry(parent)
                    .or_default()
                    .push(i.to_u32().unwrap_or(u32::MAX)),
            }
        }

        ui.label(
            RichText::new(format!("{} joint(s)", joints.len()))
                .small()
                .weak(),
        );
        ui.add_space(2.0);

        Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(egui::CornerRadius::same(3))
            .inner_margin(Margin::same(4))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(TREE_MAX_HEIGHT)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for &root in &roots {
                            draw_joint_node(ui, root, joints, &children, 0);
                        }
                    });
            });
    }
}

/// Render one joint row plus its (optionally collapsed) children.
///
/// Indents by `depth * INDENT_PX` pixels and uses the same chevron icons
/// as the scene tree panel.
fn draw_joint_node(
    ui: &mut Ui,
    joint_idx: u32,
    joints: &[SkeletonJointRecord],
    children: &HashMap<u32, Vec<u32>>,
    depth: u32,
) {
    let joint = &joints[joint_idx as usize];
    let display_name = joint.path.rsplit('/').next().unwrap_or(&joint.path);

    let child_list = children.get(&joint_idx);
    let has_children = child_list.is_some_and(|c| !c.is_empty());

    let id = ui.make_persistent_id(("skel_joint", joint_idx));
    let mut state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true);

    ui.horizontal(|ui| {
        ui.set_min_height(ROW_HEIGHT);

        // Per-level indentation.
        ui.add_space(f32::from(depth.to_u16().unwrap_or(u16::MAX)) * INDENT_PX);

        // Expand/collapse chevron — or spacer for leaf nodes.
        let (ch_rect, ch_resp) =
            ui.allocate_exact_size(egui::vec2(ICON_SIZE, ROW_HEIGHT), egui::Sense::click());
        if has_children && ui.is_rect_visible(ch_rect) {
            let col = if ch_resp.hovered() {
                ui.visuals().text_color()
            } else {
                ui.visuals().weak_text_color()
            };
            let img = egui::Image::new(icons::chevron(state.is_open()))
                .fit_to_exact_size(egui::Vec2::splat(ICON_SIZE))
                .tint(col);
            img.paint_at(
                ui,
                egui::Rect::from_center_size(ch_rect.center(), egui::Vec2::splat(ICON_SIZE)),
            );
        }
        if has_children && ch_resp.clicked() {
            state.set_open(!state.is_open());
        }

        // Joint name label — truncates when the panel is narrow.
        ui.add(Label::new(RichText::new(display_name).small()).truncate())
            .on_hover_text(&joint.path);
    });

    let is_open = state.is_open();
    state.store(ui.ctx());

    if has_children && is_open {
        for &child in child_list.unwrap() {
            draw_joint_node(ui, child, joints, children, depth + 1);
        }
    }
}

// ── Unit tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use idtx_usd::SkeletonJointRecord;
    use num_traits::ToPrimitive;

    fn make_joint(path: &str, parent: Option<u32>) -> SkeletonJointRecord {
        SkeletonJointRecord {
            path: path.to_owned(),
            parent,
            bind_transform: [0.0; 16],
            rest_transform: [0.0; 16],
        }
    }

    #[test]
    fn root_joint_has_no_parent() {
        let joint = make_joint("Hips", None);
        assert!(joint.parent.is_none());
    }

    #[test]
    fn child_joints_reference_parent_index() {
        let joints = vec![
            make_joint("Hips", None),
            make_joint("Hips/Spine", Some(0)),
            make_joint("Hips/Spine/Chest", Some(1)),
        ];
        assert_eq!(joints[1].parent, Some(0));
        assert_eq!(joints[2].parent, Some(1));
    }

    #[test]
    fn display_name_uses_last_path_segment() {
        let joint = make_joint("Hips/Spine/Chest", Some(1));
        let display = joint.path.rsplit('/').next().unwrap();
        assert_eq!(display, "Chest");
    }

    #[test]
    fn root_only_joint_has_no_children() {
        use std::collections::HashMap;
        let joints = vec![make_joint("Root", None)];
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut roots = Vec::new();
        for (i, j) in joints.iter().enumerate() {
            match j.parent {
                None => roots.push(i.to_u32().unwrap_or(u32::MAX)),
                Some(p) => children
                    .entry(p)
                    .or_default()
                    .push(i.to_u32().unwrap_or(u32::MAX)),
            }
        }
        assert_eq!(roots, vec![0]);
        assert!(children.is_empty());
    }
}
