use egui::{Align, FontId, Layout, RichText, TextStyle, Ui};
use egui_extras::{Column, TableBuilder};
use idtx_editor_model::{AuthoredTransform, InspectorMode, TransformEdit, XformEditKind};
use num_traits::ToPrimitive;

use crate::inspector::{InspectorContext, InspectorContribution, PropertyClaimSet};

const XFORM_PREFIX: &str = "xformOp:";

/// "Transform" section.
///
/// The rows shown are **Translation / Rotation / Scale** with aligned **X / Y / Z**
/// columns. How the values are sourced depends on the prim's authored
/// `xformOpOrder`, classified by the shell into [`XformEditKind`]:
///
/// - [`XformEditKind::TrsOps`] — the raw authored `translate`/`rotateXYZ`/`scale`
///   op values are read directly from [`AuthoredTransform`]. This is exact: a
///   rotation like `Y = 180°` round-trips as `180`, where decomposing the composed
///   matrix would fold it onto `X = 180°, Z = 180°` (a matrix→XYZ-Euler
///   decomposition can never place a middle-axis rotation outside ±90°).
/// - [`XformEditKind::Matrix`] — a single `xformOp:transform`. The composed matrix
///   is decomposed for display; the ambiguity above is inherent to a matrix, so
///   during a live edit the working Euler value is *held* in egui temp memory
///   (keyed by prim path) instead of being re-decomposed each frame, and a
///   recomposed matrix is authored on write. The op order is never restructured.
/// - [`XformEditKind::Unsupported`] — pivots, inverse ops, `orient`, non-`rotateXYZ`
///   rotate orders, animated ops, or a matrix mixed with other ops. The transform
///   rows are read-only with an explanatory badge.
///
/// When `authored_transform` is `None` (non-xformable, or the `authoring` feature
/// is compiled out) the panel falls back to decomposing `local_transform`.
///
/// Any change immediately authors the full TRS back to USD via
/// [`InspectorContext::author_transform`]; the confirmed gesture is journaled once
/// via [`InspectorContext::commit_transform`].
///
/// USD convention: **row-vector** (`p' = p · M`), translation in the last row.
#[derive(Debug)]
pub struct TransformPanel;

// ── InspectorContribution impl ────────────────────────────────────────────────

impl InspectorContribution for TransformPanel {
    fn label(&self) -> &'static str {
        "Transform"
    }
    fn priority(&self) -> i32 {
        -800
    }

    fn claim(&self, claims: &mut PropertyClaimSet) {
        claims.claim_prefix_editable(XFORM_PREFIX);
        claims.claim_editable("xformOpOrder");
    }

    fn present(&self, ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        ctx.info.local_transform.is_some()
            || ctx
                .properties
                .iter()
                .any(|p| p.name.starts_with(XFORM_PREFIX))
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, claims: &PropertyClaimSet) {
        let is_animated = ctx
            .properties
            .iter()
            .any(|p| p.name.starts_with(XFORM_PREFIX) && p.time_sample_count > 0);

        let identity = [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ];
        let m = ctx.info.local_transform.as_ref().unwrap_or(&identity);

        // Representation drives sourcing: read authored op values for TRS-op prims
        // (exact), decompose the matrix otherwise.
        let authored = ctx.info.authored_transform.as_ref();
        let kind = authored.map(|a| a.kind);
        let (t, r, s) = seed_trs(authored, m, ctx.info.is_locally_authored);

        let unsupported = kind == Some(XformEditKind::Unsupported);
        let editable =
            ctx.is_prim_editable() && claims.is_property_editable(XFORM_PREFIX) && !unsupported;

        let authoring = matches!(ctx.mode, InspectorMode::Authoring);

        // Show a warning badge if the prim is in authoring mode but not
        // locally authored (i.e. inside a packed stage without an override).
        if authoring && !ctx.info.is_locally_authored {
            warn_badge(ui, "! Read-only: prim has no root-layer opinion !");
        }

        // Explain why an unsupported op stack cannot be edited here.
        if authoring && unsupported {
            let diag = authored
                .and_then(|a| a.diagnostic.as_deref())
                .unwrap_or("transform op stack is not editable");
            warn_badge(ui, &format!("! Read-only: {diag} !"));
        }

        // "Reset Transform": remove every transform opinion (`xformOp:*` /
        // `xformOpOrder`) authored in the open stage's root layer, reverting the
        // prim to whatever weaker layers / schema compose. Offered whenever the
        // prim is editable and carries at least one root-authored transform op —
        // even for an *unsupported* op stack, where the TRS table above is
        // read-only but the local override can still be cleared. Recorded as one
        // journal entry.
        let resettable: Vec<String> = ctx
            .properties
            .iter()
            .filter(|p| {
                p.root_authored && (p.name == "xformOpOrder" || p.name.starts_with(XFORM_PREFIX))
            })
            .map(|p| p.name.clone())
            .collect();
        if authoring && ctx.is_prim_editable() && !resettable.is_empty() {
            let resp = ui.button("Reset Transform").on_hover_text(
                "Remove this prim's transform opinion authored in the open stage's \
                     root layer, reverting to the composed transform (undoable)",
            );
            if resp.clicked() {
                ctx.reset_transform(resettable);
            }
            ui.add_space(2.0);
        }

        let small_px = ui.text_style_height(&TextStyle::Small);
        let row_h = small_px + 6.0;
        let drag_h = small_px + 4.0;

        let clip_w = ui.clip_rect().width() * 0.8;
        let label_w = clip_w * 0.40;
        let val_w = clip_w * 0.20;

        let width_bucket = (clip_w / 8.0).to_u32().unwrap_or(u32::MAX);
        ui.push_id(width_bucket, |ui| {
            let mono = FontId::monospace(small_px);

            if editable {
                // Matrix prims: hold the Euler working value during a live edit so
                // it is not re-folded by re-decomposition each frame. TRS-op prims
                // read exact authored values every frame, so no hold is needed and
                // the pre-edit authored TRS is journaled for exact undo/redo.
                let hold = kind == Some(XformEditKind::Matrix);
                let authored_seed =
                    (kind == Some(XformEditKind::TrsOps)).then_some(TransformEdit {
                        translation: t,
                        rotation_xyz_degrees: r,
                        scale: s,
                    });
                draw_editable_trs(
                    ui,
                    ctx,
                    t,
                    r,
                    s,
                    row_h,
                    drag_h,
                    label_w,
                    val_w,
                    is_animated,
                    hold,
                    authored_seed,
                );
            } else {
                draw_readonly_trs(ui, t, r, s, row_h, label_w, val_w, &mono, is_animated);
            }
        });
    }
}

/// Render a small warning-coloured badge line.
fn warn_badge(ui: &mut Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(text)
                .small()
                .color(ui.visuals().warn_fg_color),
        );
    });
    ui.add_space(2.0);
}

/// Source the display/edit TRS for the prim.
///
/// Prefers the exact authored op values for [`XformEditKind::TrsOps`] prims —
/// but only when the prim is **locally authored**. A referenced-only prim has no
/// root-layer op stack, so the classifier reports an empty stack as a `TrsOps`
/// identity default (see `classify_authored_transform`); trusting that would show
/// identity even though the composed matrix `m` carries the real pose from the
/// reference. In that case (and for matrix/unsupported prims) we decompose `m`
/// instead — the Euler triple may differ from any authored one, but the
/// orientation and translation are preserved.
fn seed_trs(
    authored: Option<&AuthoredTransform>,
    m: &[[f32; 4]; 4],
    is_locally_authored: bool,
) -> ([f64; 3], [f64; 3], [f64; 3]) {
    if is_locally_authored
        && let Some(a) = authored
        && a.kind == XformEditKind::TrsOps
    {
        return (a.translation, a.rotation_xyz_degrees, a.scale);
    }
    let (t, r, s) = decompose_trs(m);
    (widen(t), widen(r), widen(s))
}

/// Lossless `[f32; 3]` → `[f64; 3]`.
fn widen(v: [f32; 3]) -> [f64; 3] {
    [f64::from(v[0]), f64::from(v[1]), f64::from(v[2])]
}

// ── Read-only TRS table ───────────────────────────────────────────────────────
#[allow(clippy::too_many_arguments)]
fn draw_readonly_trs(
    ui: &mut Ui,
    t: [f64; 3],
    r: [f64; 3],
    s: [f64; 3],
    row_h: f32,
    label_w: f32,
    val_w: f32,
    mono: &FontId,
    is_animated: bool,
) {
    TableBuilder::new(ui)
        .vscroll(false)
        .column(Column::initial(label_w).clip(true))
        .column(Column::initial(val_w))
        .column(Column::initial(val_w))
        .column(Column::initial(val_w))
        .header(row_h, |mut header| {
            header.col(|_ui| {});
            for axis in ["X", "Y", "Z"] {
                header.col(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(axis).weak().strong());
                    });
                });
            }
        })
        .body(|mut body| {
            for (label, vals) in [("Translate", t), ("Rotate °", r), ("Scale", s)] {
                body.row(row_h, |mut row| {
                    row.col(|ui| {
                        ui.horizontal(|ui| {
                            if is_animated {
                                ui.label(
                                    RichText::new("~").weak().color(ui.visuals().warn_fg_color),
                                );
                            }
                            ui.add(egui::Label::new(RichText::new(label).strong()).truncate());
                        });
                    });
                    for v in vals {
                        row.col(|ui| {
                            let mut text = fmt_f64(v);
                            ui.add(
                                egui::TextEdit::singleline(&mut text)
                                    .font(mono.clone())
                                    .horizontal_align(Align::RIGHT)
                                    .interactive(false)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    }
                });
            }
        });
}

// ── Editable TRS table ────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments, clippy::float_cmp)]
fn draw_editable_trs(
    ui: &mut Ui,
    ctx: &mut InspectorContext<'_>,
    t: [f64; 3],
    r: [f64; 3],
    s: [f64; 3],
    row_h: f32,
    drag_h: f32,
    label_w: f32,
    val_w: f32,
    is_animated: bool,
    hold: bool,
    authored_seed: Option<TransformEdit>,
) {
    use std::cell::Cell;

    // Pre-edit local matrix (USD row-major); the pair captured/committed below
    // journals a single before→after entry for the whole edit gesture.
    let local_mat = ctx.info.local_transform.unwrap_or([
        [1., 0., 0., 0.],
        [0., 1., 0., 0.],
        [0., 0., 1., 0.],
        [0., 0., 0., 1.],
    ]);

    let path = ctx.info.path.clone();

    // For matrix prims, hold the working Euler value across frames so a live drag
    // is not re-seeded (and re-folded) from the re-decomposed matrix. TRS-op prims
    // read exact authored values each frame, so they seed directly from (t, r, s).
    let work_id = egui::Id::new(("xform_edit_work", path.as_str()));
    let stored_work: Option<[[f64; 3]; 3]> = if hold {
        ui.data(|d| d.get_temp(work_id))
    } else {
        None
    };
    let [mut new_t, mut new_r, mut new_s] = stored_work.unwrap_or([t, r, s]);

    // Edit-gesture tracking. `edit_started` fires when any field first gains
    // focus or a drag begins; `committed` fires when a field loses focus, Enter
    // is pressed, or a drag ends — the moment the value is "confirmed".  Cells
    // let the per-row table closures share these flags without aliasing borrows.
    let edit_started = Cell::new(false);
    let committed = Cell::new(false);

    let drag = |ui: &mut Ui, comp: &mut f64, speed: f64, decimals: usize| {
        let resp = ui.add(
            egui::DragValue::new(comp)
                .speed(speed)
                .max_decimals(decimals),
        );
        if resp.gained_focus() || resp.drag_started() {
            edit_started.set(true);
        }
        if resp.lost_focus() || resp.drag_stopped() {
            committed.set(true);
        }
    };

    TableBuilder::new(ui)
        .vscroll(false)
        .column(Column::initial(label_w).clip(true))
        .column(Column::initial(val_w))
        .column(Column::initial(val_w))
        .column(Column::initial(val_w))
        .header(row_h, |mut header| {
            header.col(|_ui| {});
            for axis in ["X", "Y", "Z"] {
                header.col(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(axis).weak().strong());
                    });
                });
            }
        })
        .body(|mut body| {
            // Translation row
            body.row(drag_h, |mut row| {
                row.col(|ui| {
                    ui.horizontal(|ui| {
                        if is_animated {
                            ui.label(RichText::new("~").weak().color(ui.visuals().warn_fg_color));
                        }
                        ui.add(egui::Label::new(RichText::new("Translate").strong()).truncate());
                    });
                });
                for comp in &mut new_t {
                    row.col(|ui| drag(ui, comp, 0.01, 4));
                }
            });

            // Rotation row (degrees)
            body.row(drag_h, |mut row| {
                row.col(|ui| {
                    ui.add(egui::Label::new(RichText::new("Rotate °").strong()).truncate());
                });
                for comp in &mut new_r {
                    row.col(|ui| drag(ui, comp, 0.1, 3));
                }
            });

            // Scale row
            body.row(drag_h, |mut row| {
                row.col(|ui| {
                    ui.add(egui::Label::new(RichText::new("Scale").strong()).truncate());
                });
                for comp in &mut new_s {
                    row.col(|ui| drag(ui, comp, 0.001, 4));
                }
            });
        });

    let changed = new_t != t || new_r != r || new_s != s;

    let after = TransformEdit {
        translation: new_t,
        rotation_xyz_degrees: new_r,
        scale: new_s,
    };

    // Persist the pre-edit matrix (and, for TRS-op prims, the pre-edit authored
    // Euler TRS) for the *whole* gesture so the commit can journal a single
    // before→after pair.  Keyed by prim path (stable across frames and
    // panel-width buckets); the current-frame values are the pre-edit values
    // because live edits are only applied when the shell drains actions.
    let before_id = egui::Id::new(("xform_edit_before", path.as_str()));
    let before_trs_id = egui::Id::new(("xform_edit_before_trs", path.as_str()));
    let stored_before: Option<[[f32; 4]; 4]> = ui.data(|d| d.get_temp(before_id));
    if stored_before.is_none() && (edit_started.get() || changed) {
        ui.data_mut(|d| d.insert_temp(before_id, local_mat));
        if let Some(seed) = authored_seed {
            ui.data_mut(|d| d.insert_temp(before_trs_id, seed));
        }
    }

    // Hold the working Euler value for matrix prims until the gesture confirms.
    if hold {
        if committed.get() {
            ui.data_mut(|d| d.remove::<[[f64; 3]; 3]>(work_id));
        } else if edit_started.get() || changed || stored_work.is_some() {
            ui.data_mut(|d| d.insert_temp(work_id, [new_t, new_r, new_s]));
        }
    }

    // Stream the live value to USD every frame it changes (immediate viewport
    // feedback); this path never touches the journal.
    if changed {
        ctx.author_transform(after);
    }

    // On confirm, record the gesture in the journal and end the edit session.
    if committed.get() {
        let before = stored_before.unwrap_or(local_mat);
        let before_trs: Option<TransformEdit> = ui.data(|d| d.get_temp(before_trs_id));
        ctx.commit_transform(before, after, before_trs);
        ui.data_mut(|d| d.remove::<[[f32; 4]; 4]>(before_id));
        ui.data_mut(|d| d.remove::<TransformEdit>(before_trs_id));
    }
}

// ── Number formatting ─────────────────────────────────────────────────────────

/// Format a float: integers without noise, fractional values to 3 dp.
fn fmt_f64(v: f64) -> String {
    if v.is_nan() || v.is_infinite() {
        return format!("{v}");
    }
    if v.fract().abs() < 5e-4 && v.abs() < 1_000_000.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.3}")
    }
}

// ── TRS decomposition ─────────────────────────────────────────────────────────
//
// Input: 4×4 row-major matrix from UsdGeomXformable::GetLocalTransformation().
// USD row-vector convention: p' = p · M, translation in last row m[3][0..2].
//
// For a standard SRT composition (scale → rotate → translate):
//   Upper-left 3×3 holds S·R where row i has magnitude s_i.
//   Translation is always in row 3.
//
// NOTE: this is only the fallback for matrix-authored prims. The `asin` branch
// caps the extracted middle-axis (Y) angle at ±90°, so it can never recover a
// `Y = 180°` rotation — that is intrinsic to a matrix, not a bug here. TRS-op
// prims bypass this entirely by reading the authored `rotateXYZ` op value.

fn decompose_trs(m: &[[f32; 4]; 4]) -> ([f32; 3], [f32; 3], [f32; 3]) {
    // Translation — last row.
    let t = [m[3][0], m[3][1], m[3][2]];

    // Scale — magnitude of each row of the upper-left 3×3.
    let row_len =
        |i: usize| -> f32 { (m[i][0] * m[i][0] + m[i][1] * m[i][1] + m[i][2] * m[i][2]).sqrt() };
    let sx = row_len(0).max(f32::EPSILON);
    let sy = row_len(1).max(f32::EPSILON);
    let sz = row_len(2).max(f32::EPSILON);
    let s = [sx, sy, sz];

    // Rotation — normalize rows to remove scale, then extract XYZ Euler angles.
    //
    // For M_XYZ = Rx · Ry · Rz (row-vector convention), the closed-form is:
    //   R[0][2] = −sin(ry)
    //   R[1][2] =  sin(rx)·cos(ry)    R[2][2] = cos(rx)·cos(ry)
    //   R[0][0] =  cos(ry)·cos(rz)    R[0][1] = cos(ry)·sin(rz)
    let r = [
        [m[0][0] / sx, m[0][1] / sx, m[0][2] / sx],
        [m[1][0] / sy, m[1][1] / sy, m[1][2] / sy],
        [m[2][0] / sz, m[2][1] / sz, m[2][2] / sz],
    ];

    let ry_rad = (-r[0][2]).clamp(-1., 1.).asin();
    let cos_ry = ry_rad.cos();

    let (rx_rad, rz_rad) = if cos_ry.abs() > 1e-6 {
        let rx = (r[1][2] / cos_ry).atan2(r[2][2] / cos_ry);
        let rz = (r[0][1] / cos_ry).atan2(r[0][0] / cos_ry);
        (rx, rz)
    } else {
        // Gimbal lock: ry ≈ ±90°. Determine rx−rz (or rx+rz) only.
        let rx = r[1][0].atan2(r[1][1]);
        (rx, 0.0_f32)
    };

    let rot = [
        rx_rad.to_degrees(),
        ry_rad.to_degrees(),
        rz_rad.to_degrees(),
    ];
    (t, rot, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trsops(rotation_xyz_degrees: [f64; 3]) -> AuthoredTransform {
        AuthoredTransform {
            kind: XformEditKind::TrsOps,
            translation: [0.0; 3],
            rotation_xyz_degrees,
            scale: [1.0; 3],
            diagnostic: None,
        }
    }

    /// The core regression: a `rotateXYZ = (0, 180, 0)` op reads back as `Y = 180`,
    /// not the folded `X = 180, Z = 180` a matrix decomposition would yield.
    #[test]
    fn trsops_reads_authored_y180_exactly() {
        let authored = trsops([0.0, 180.0, 0.0]);
        // A matrix that would decompose to the folded triple — must be ignored.
        let folded = [
            [-1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., -1., 0.],
            [0., 0., 0., 1.],
        ];
        let (_t, r, _s) = seed_trs(Some(&authored), &folded, true);
        assert!((r[0]).abs() < 1e-9, "X must stay 0, got {}", r[0]);
        assert!((r[1] - 180.0).abs() < 1e-9, "Y must be 180, got {}", r[1]);
        assert!((r[2]).abs() < 1e-9, "Z must stay 0, got {}", r[2]);
    }

    /// Matrix / unsupported / absent classification falls back to decomposing the
    /// composed matrix (the fallback is the only option a matrix admits).
    #[test]
    fn matrix_kind_falls_back_to_decompose() {
        let authored = AuthoredTransform {
            kind: XformEditKind::Matrix,
            translation: [9.0; 3], // must be ignored in favour of the matrix
            rotation_xyz_degrees: [9.0; 3],
            scale: [9.0; 3],
            diagnostic: None,
        };
        let identity = [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ];
        let (t, r, s) = seed_trs(Some(&authored), &identity, true);
        assert_eq!(t, [0.0, 0.0, 0.0]);
        assert_eq!(r, [0.0, 0.0, 0.0]);
        assert_eq!(s, [1.0, 1.0, 1.0]);
    }

    #[test]
    fn none_authored_decomposes_matrix() {
        // 90° about Y (row-vector Ry): m[0][2] = -sin(90) = -1, m[2][0] = sin(90) = 1.
        let ry90 = [
            [0., 0., -1., 0.],
            [0., 1., 0., 0.],
            [1., 0., 0., 0.],
            [0., 0., 0., 1.],
        ];
        let (_t, r, _s) = seed_trs(None, &ry90, false);
        assert!((r[1] - 90.0).abs() < 1e-3, "Y ≈ 90°, got {}", r[1]);
    }

    /// Regression for the referenced-only camera rig: a prim whose pose comes
    /// entirely from a reference has no root-layer op stack, so the classifier
    /// reports an empty stack as a `TrsOps` identity default. The composed matrix
    /// still carries the real pose (here `Y = 1.5`). Because the prim is **not**
    /// locally authored, `seed_trs` must ignore the identity default and decompose
    /// the matrix — otherwise the inspector shows identity for a camera that is
    /// actually seated at eye height.
    #[test]
    fn referenced_only_matrix_prim_decomposes_not_identity() {
        // Classifier's default for an empty op stack: TrsOps identity.
        let authored = trsops([0.0, 0.0, 0.0]);
        // The composed local matrix from the reference: translate Y = 1.5
        // (USD row-vector convention → translation in row 3).
        let composed = [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 1.5, 0., 1.],
        ];
        let (t, r, s) = seed_trs(Some(&authored), &composed, false);
        assert!(
            (t[1] - 1.5).abs() < 1e-6,
            "Y translation must decompose to 1.5, got {}",
            t[1]
        );
        assert_eq!(t[0], 0.0);
        assert_eq!(t[2], 0.0);
        for axis in r {
            assert!(axis.abs() < 1e-4, "rotation must stay 0, got {axis}");
        }
        for axis in s {
            assert!((axis - 1.0).abs() < 1e-4, "scale must stay 1, got {axis}");
        }
    }

    #[test]
    fn decompose_identity_is_zero_rotation() {
        let identity = [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ];
        let (t, r, s) = decompose_trs(&identity);
        assert_eq!(t, [0.0, 0.0, 0.0]);
        for axis in r {
            assert!(axis.abs() < 1e-4);
        }
        assert!((s[0] - 1.0).abs() < 1e-4);
        assert!((s[1] - 1.0).abs() < 1e-4);
        assert!((s[2] - 1.0).abs() < 1e-4);
    }
}
