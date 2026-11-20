//! "Collision" section: the `OpenUSD`-standard `UsdPhysicsCollisionAPI`
//! (`physics:collisionEnabled`) + `UsdPhysicsMeshCollisionAPI`
//! (`physics:approximation`), plus the reduced `IdtxCollisionAPI` addendum
//! (`idtx:collision:channel` and the pre-baked `idtx:collision:hull:*`).
//!
//! Mirrors the light panel's shadow-section pattern. Display (attribute values +
//! the viewport wireframe) works in every editor product; **authoring is
//! conductor-only** ([`InspectorContext::is_cookable`]), so the interactive
//! widgets and the buttons are gated on it. Inspector/poser render the data
//! read-only.
//!
//! Attribute presentation:
//! - `physics:collisionEnabled` → checkbox
//! - mesh collider → `physics:approximation` dropdown over the standard tokens;
//!   analytic typed gprims (Sphere/Cube/Cylinder/Cone/Capsule) show their
//!   collider shape derived read-only from the gprim type (the collider is sized
//!   from the gprim's own geometry — no authored dimensions)
//! - `physics:approximation == convexHull` → the pre-baked hull array rows + a
//!   **Generate `CollisionHull`** button
//! - `idtx:collision:channel` → a checkbox per allowed channel (checked when
//!   present)
//! - no collider yet, prim xformable → an **Add Collision** button

use egui::Ui;

use crate::inspector::{
    widgets::attribute_row, InspectorContext, InspectorContribution, PropertyClaimSet,
};
use idtx_editor_model::{AttributeValue, HullGenParams};

// Schema vocabulary (kept local so this pure-UI crate needs no USD dependency;
// must stay in sync with crates/idtx-usd-extension).
const PHYSICS_COLLISION_SCHEMA: &str = "PhysicsCollisionAPI";
/// `IdtxCollisionBodyAPI` — the whole-asset interaction/body grouping marker
/// (applied to a grouping Xformable, not a collider gprim).
const COLLISION_BODY_SCHEMA: &str = "IdtxCollisionBodyAPI";
/// `UsdPhysicsRigidBodyAPI` — the `OpenUSD`-standard dynamic-body marker. Applied
/// to the body root (an Xformable or collider gprim); the runtime physics
/// subsystem simulates prims carrying it (see `idtx-studio-session::physics`).
/// Must stay in sync with `idtx_usd_extension::PHYSICS_RIGID_BODY_SCHEMA`.
const PHYSICS_RIGID_BODY_SCHEMA: &str = "PhysicsRigidBodyAPI";
const PHYSICS_PREFIX: &str = "physics:";
const COLLISION_PREFIX: &str = "idtx:collision:";
const ATTR_PHYSICS_COLLISION_ENABLED: &str = "physics:collisionEnabled";
const ATTR_PHYSICS_APPROXIMATION: &str = "physics:approximation";
const ATTR_HULL_POINTS: &str = "idtx:collision:hull:points";
const ATTR_HULL_TRIANGLES: &str = "idtx:collision:hull:triangles";
const ATTR_CHANNEL: &str = "idtx:collision:channel";
const HULL_MAX_VERTICES: u32 = 96;
/// `physics:approximation` allowed tokens (vendored `meshCollisionAPI.h`).
const APPROXIMATION_TOKENS: &[&str] = &[
    "none",
    "convexDecomposition",
    "convexHull",
    "boundingSphere",
    "boundingCube",
    "meshSimplification",
];
const CHANNEL_TOKENS: &[&str] = &["All", "Physics", "Interaction"];

/// What the collision panel should show for the selected prim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollisionSection {
    /// No collider and nothing to offer (not xformable, or not conductor).
    Hidden,
    /// Xformable prim without the collider schema, in conductor → offer "Add
    /// Collision".
    OfferAdd,
    /// Collider applied → show the collision attributes; `editable` iff conductor.
    Present { editable: bool },
}

/// Decide the panel state from the gating inputs.
///
/// `conductor_editable` is `is_cookable() && is_prim_editable()` — all collision
/// *authoring* is restricted to `idtx-conductor`. Display of an applied collider
/// works everywhere (`Present { editable: false }` in inspector/poser).
fn collision_section(
    has_collider: bool,
    is_xformable: bool,
    conductor_editable: bool,
) -> CollisionSection {
    if has_collider {
        CollisionSection::Present {
            editable: conductor_editable,
        }
    } else if is_xformable && conductor_editable {
        CollisionSection::OfferAdd
    } else {
        CollisionSection::Hidden
    }
}

/// Whether `channel_summary` (a `token[]` value summary like `[Physics, All]`)
/// lists `token`. Splits on non-alphanumerics so quoting/bracketing is ignored.
fn channel_is_set(channel_summary: &str, token: &str) -> bool {
    channel_summary
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word == token)
}

/// Parse a `bool` value summary (`true`/`false`/`1`/`0`), defaulting to `true`
/// (the schema default) when absent/unparseable.
fn parse_enabled(summary: &str) -> bool {
    !matches!(summary.trim(), "false" | "False" | "0")
}

/// The collider shape derived read-only from an analytic gprim's own USD type,
/// matching `idtx_scene::collision::resolve_collision_shape`'s gprim-type branch.
/// Non-analytic / untyped prims collide against their mesh.
fn derived_analytic_label(type_name: &str) -> &'static str {
    match type_name {
        "Sphere" => "sphere",
        "Cube" => "cube",
        "Cylinder" => "cylinder",
        "Cone" => "cone",
        "Capsule" => "capsule",
        "Plane" => "plane (static half-space)",
        _ => "mesh",
    }
}

/// Whether `type_name` is a geometry gprim — a valid *collider* target (the
/// standard physics parser only recognises a collider on a `UsdGeomGprim`). A
/// non-gprim `Xformable` (Xform/Scope/…) is instead a candidate *collision
/// body* grouping prim. Mirrors the gprim set `resolve_collision_shape` keys on
/// plus the other common concrete gprims.
fn is_gprim(type_name: &str) -> bool {
    matches!(
        type_name,
        "Mesh"
            | "Cube"
            | "Sphere"
            | "Cylinder"
            | "Cone"
            | "Capsule"
            | "Points"
            | "Plane"
            | "BasisCurves"
            | "NurbsCurves"
    )
}

#[derive(Debug)]
pub struct CollisionPanel;

impl InspectorContribution for CollisionPanel {
    fn label(&self) -> &'static str {
        "Collision"
    }
    fn priority(&self) -> i32 {
        0
    }
    fn claim(&self, claims: &mut PropertyClaimSet) {
        // Own the standard physics:* collision attrs and every idtx:collision:*
        // property so the fallback panel doesn't duplicate them.
        claims.claim_prefix(PHYSICS_PREFIX);
        claims.claim_prefix(COLLISION_PREFIX);
    }

    fn present(&self, ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        ctx.has_applied_schema(PHYSICS_COLLISION_SCHEMA)
            || ctx.has_applied_schema(COLLISION_BODY_SCHEMA)
            || ctx.has_applied_schema(PHYSICS_RIGID_BODY_SCHEMA)
            || (ctx.info.local_transform.is_some() && ctx.is_cookable() && ctx.is_prim_editable())
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, _claims: &PropertyClaimSet) {
        let has_collider = ctx.has_applied_schema(PHYSICS_COLLISION_SCHEMA);
        let has_body = ctx.has_applied_schema(COLLISION_BODY_SCHEMA);
        let has_rigid_body = ctx.has_applied_schema(PHYSICS_RIGID_BODY_SCHEMA);
        let is_xformable = ctx.info.local_transform.is_some();
        let is_gprim = is_gprim(&ctx.info.type_name);
        // A UsdGeomPlane is an unbounded static surface: it may be a collider but
        // can never be a rigid body or whole-asset collision body (the runtime
        // physics engine treats it as a static half-space). Gate those markers off.
        let is_plane = ctx.info.type_name == "Plane";
        let conductor_editable = ctx.is_cookable() && ctx.is_prim_editable();

        // Collider section — collision *geometry*, on a gprim.
        match collision_section(has_collider, is_xformable && is_gprim, conductor_editable) {
            CollisionSection::Hidden => {}
            CollisionSection::OfferAdd => {
                if ui.button("Add Collision").clicked() {
                    // One atomic intent: the session applies the standard collider
                    // schemas (+ reduced addendum) as a failure-atomic batch.
                    ctx.apply_collision_api();
                }
            }
            CollisionSection::Present { editable } => {
                if editable && ui.button("Remove Collision").clicked() {
                    // One atomic intent: the session removes the applied collider
                    // schemas and clears their authored opinions (channel + the
                    // bulky pre-baked hull arrays) in one failure-atomic batch.
                    ctx.remove_collision_api();
                }
                draw_present(ui, ctx, editable);
            }
        }

        // Collision-body section — the whole-asset *interaction unit*, applied
        // to a grouping Xformable that is not itself a collider gprim.
        draw_collision_body(ui, ctx, has_body, is_xformable, is_gprim, is_plane, conductor_editable);

        // Rigid-body section — the OpenUSD-standard dynamic-body marker driving
        // the runtime physics simulation. Independent of the collider / body
        // markers above (same conductor-only edit rules).
        draw_rigid_body(ui, ctx, has_rigid_body, is_xformable, is_gprim, is_plane, conductor_editable);
    }
}

/// The whole-asset collision/interaction body (`IdtxCollisionBodyAPI`): a
/// property-less marker on a grouping `Xformable`. A raycast hit on any
/// descendant collider resolves up to this prim (host-side `collision_body`),
/// so gaze / highlight / grab act on the whole asset. Authored as a single
/// `ApplyApiSchema` (undo removes it), gated to conductor like the collider.
fn draw_collision_body(
    ui: &mut Ui,
    ctx: &mut InspectorContext<'_>,
    has_body: bool,
    is_xformable: bool,
    is_gprim: bool,
    is_plane: bool,
    conductor_editable: bool,
) {
    if has_body {
        ui.label(
            egui::RichText::new(
                "✓ Collision body — gaze / highlight / grab act on the whole subtree",
            )
            .small(),
        );
        let clicked = ui
            .button("Remove collision body")
            .on_hover_text(
                "Remove IdtxCollisionBodyAPI.",
            )
            .clicked();
        if clicked {
            ctx.remove_api_schema(COLLISION_BODY_SCHEMA);
        }
        return;
    }
    // Offer on a grouping Xformable and a collider gprim, in conductor.
    if offers_body_marker(is_xformable, is_gprim, conductor_editable) {
        if !body_marker_interactive(is_plane) {
            // A Plane is always a static collider — never a collision body.
            ui.add_enabled(false, egui::Button::new("Mark as collision body"))
                .on_hover_text(
                    "A Plane is an unbounded static surface — it is always a static \
                     collider and cannot be a collision body.",
                );
            return;
        }
        let clicked = ui
            .button("Mark as collision body")
            .on_hover_text(
                "Apply IdtxCollisionBodyAPI: a raycast hit on any descendant \
                 collider resolves to this prim, so the whole asset highlights \
                 as one. Carries no geometry.",
            )
            .clicked();
        if clicked {
            ctx.apply_api_schema(COLLISION_BODY_SCHEMA);
        }
    }
}

/// Whether a whole-asset body marker (collision body / rigid body) may be
/// *offered* on this prim: a grouping Xformable or a collider gprim, and only in
/// conductor (all physics authoring is conductor-only). Both the collision-body
/// and rigid-body sections gate their "Mark …" buttons on this.
fn offers_body_marker(is_xformable: bool, is_gprim: bool, conductor_editable: bool) -> bool {
    (is_xformable || is_gprim) && conductor_editable
}

/// Whether an *offered* body marker (see [`offers_body_marker`]) should be
/// interactable. A `UsdGeomPlane` is always a static collider — the runtime
/// physics engine treats it as an unbounded static half-space — so its rigid-body
/// and collision-body markers are shown disabled (with a hover-why) rather than
/// active. Returns `false` for a plane, `true` otherwise.
fn body_marker_interactive(is_plane: bool) -> bool {
    !is_plane
}

/// The `OpenUSD`-standard rigid body (`UsdPhysicsRigidBodyAPI`): the marker the
/// runtime physics subsystem keys on to simulate a prim as a dynamic body.
/// Applied to the body root (an Xformable or a collider gprim). A property-less
/// apply here (defaults: `physics:rigidBodyEnabled = true`, non-kinematic);
/// authored as a single `ApplyApiSchema` (undo removes it), gated to conductor
/// exactly like the collider / collision-body sections above.
fn draw_rigid_body(
    ui: &mut Ui,
    ctx: &mut InspectorContext<'_>,
    has_rigid_body: bool,
    is_xformable: bool,
    is_gprim: bool,
    is_plane: bool,
    conductor_editable: bool,
) {
    if has_rigid_body {
        ui.label(
            egui::RichText::new("✓ Rigid body — simulated as a dynamic body at runtime").small(),
        );
        let clicked = ui
            .button("Remove Rigid Body")
            .on_hover_text("Remove PhysicsRigidBodyAPI.")
            .clicked();
        if clicked {
            ctx.remove_api_schema(PHYSICS_RIGID_BODY_SCHEMA);
        }
        return;
    }
    // Offer on a grouping Xformable or a collider gprim, in conductor.
    if offers_body_marker(is_xformable, is_gprim, conductor_editable) {
        if !body_marker_interactive(is_plane) {
            // A Plane is an unbounded static surface — never a dynamic rigid body.
            ui.add_enabled(false, egui::Button::new("Mark Rigid Body"))
                .on_hover_text(
                    "A Plane is an unbounded static surface — it is always a static \
                     collider and cannot be a rigid body.",
                );
            return;
        }
        let clicked = ui
            .button("Mark Rigid Body")
            .on_hover_text(
                "Apply PhysicsRigidBodyAPI: the runtime physics simulation drives \
                 this prim as a dynamic rigid body (gravity, collisions). Pair it \
                 with a collider so it interacts with the scene.",
            )
            .clicked();
        if clicked {
            ctx.apply_api_schema(PHYSICS_RIGID_BODY_SCHEMA);
        }
    }
}

/// Render the attributes of an applied collider. Read-only unless `editable`
/// (conductor); edits author through `commit_attribute` (which both applies and
/// journals a single undo entry).
fn draw_present(ui: &mut Ui, ctx: &mut InspectorContext<'_>, editable: bool) {
    // Read current values up front (field access on `ctx.properties` yields refs
    // with the context's data lifetime, so they don't borrow `ctx` itself).
    let enabled = ctx
        .properties
        .iter()
        .find(|p| p.name == ATTR_PHYSICS_COLLISION_ENABLED)
        .is_none_or(|p| parse_enabled(&p.value_summary));
    let approximation = ctx
        .properties
        .iter()
        .find(|p| p.name == ATTR_PHYSICS_APPROXIMATION)
        .map_or_else(
            || "none".to_owned(),
            |p| p.value_summary.trim().trim_matches('"').to_owned(),
        );
    let channel_summary = ctx
        .properties
        .iter()
        .find(|p| p.name == ATTR_CHANNEL)
        .map(|p| p.value_summary.clone())
        .unwrap_or_default();
    // Whether this is a mesh collider (approximation applies) vs an analytic
    // typed gprim (shape derived from the gprim type, sized from its geometry).
    let prim_type = ctx.info.type_name.clone();
    let is_mesh = prim_type == "Mesh";

    // physics:collisionEnabled → checkbox.
    if editable {
        let mut v = enabled;
        if ui.checkbox(&mut v, "enabled").changed() {
            ctx.commit_attribute(
                ATTR_PHYSICS_COLLISION_ENABLED,
                Some(AttributeValue::Bool(enabled)),
                AttributeValue::Bool(v),
            );
        }
    } else {
        ui.label(egui::RichText::new(format!("enabled: {enabled}")).small());
    }

    if is_mesh {
        // Mesh collider → physics:approximation dropdown. The approximation we
        // react to is the live selection, so the panel updates at once.
        let active = if editable {
            let mut sel = approximation.clone();
            egui::ComboBox::from_label("approximation")
                .selected_text(&sel)
                .show_ui(ui, |ui| {
                    for &tok in APPROXIMATION_TOKENS {
                        ui.selectable_value(&mut sel, tok.to_owned(), tok);
                    }
                });
            if sel != approximation {
                ctx.commit_attribute(
                    ATTR_PHYSICS_APPROXIMATION,
                    Some(AttributeValue::Token(approximation.clone())),
                    AttributeValue::Token(sel.clone()),
                );
            }
            sel
        } else {
            ui.label(egui::RichText::new(format!("approximation: {approximation}")).small());
            approximation.clone()
        };
        draw_mesh_approximation_body(ui, ctx, editable, &active);
    } else {
        // Analytic typed gprim: the collider shape + dimensions are derived from
        // the gprim's own geometry (read-only — no authored collider dimensions).
        let derived = derived_analytic_label(&prim_type);
        ui.label(
            egui::RichText::new(format!("shape: {derived} (sized from the gprim geometry)"))
                .small()
                .weak(),
        );
    }

    // channel → one checkbox per allowed channel; toggling authors the whole
    // token[] array on the reduced IdtxCollisionAPI addendum.
    ui.separator();
    ui.label(egui::RichText::new("channel").small().weak());
    let current: Vec<String> = CHANNEL_TOKENS
        .iter()
        .filter(|t| channel_is_set(&channel_summary, t))
        .map(|t| (*t).to_owned())
        .collect();
    for &tok in CHANNEL_TOKENS {
        let mut on = current.iter().any(|c| c == tok);
        let resp = if editable {
            ui.checkbox(&mut on, tok)
        } else {
            ui.add_enabled(false, egui::Checkbox::new(&mut on, tok))
        };
        if editable && resp.changed() {
            let mut next = current.clone();
            if on {
                if !next.iter().any(|c| c == tok) {
                    next.push(tok.to_owned());
                }
            } else {
                next.retain(|c| c != tok);
            }
            ctx.commit_attribute(
                ATTR_CHANNEL,
                Some(AttributeValue::TokenArray(current.clone())),
                AttributeValue::TokenArray(next),
            );
        }
    }
}

/// The body of a mesh collider under the chosen `approximation`: the pre-baked
/// hull rows + generate controls for `convexHull`, else a short descriptive note.
fn draw_mesh_approximation_body(
    ui: &mut Ui,
    ctx: &mut InspectorContext<'_>,
    editable: bool,
    approximation: &str,
) {
    match approximation {
        "convexHull" => {
            // Pre-baked hull geometry rows (element counts) + the generate
            // controls. The runtime consumes the hull verbatim — it is never
            // regenerated at runtime.
            if let Some(p) = ctx.properties.iter().find(|p| p.name == ATTR_HULL_POINTS) {
                attribute_row(ui, ctx, p);
            }
            if let Some(p) = ctx.properties.iter().find(|p| p.name == ATTR_HULL_TRIANGLES) {
                attribute_row(ui, ctx, p);
            }
            if editable {
                draw_hull_generate(ui, ctx);
            }
        }
        "none" => {
            ui.label(
                egui::RichText::new("collides against the prim's own mesh geometry")
                    .small()
                    .weak(),
            );
        }
        "boundingSphere" | "boundingCube" => {
            ui.label(
                egui::RichText::new("analytic collider sized from the mesh bounds")
                    .small()
                    .weak(),
            );
        }
        _ => {
            // convexDecomposition / meshSimplification: no dedicated IDTX
            // narrow-phase yet — the runtime collides against the mesh.
            ui.label(
                egui::RichText::new("no IDTX narrow-phase yet — collides against the mesh")
                    .small()
                    .weak(),
            );
        }
    }
}

/// The hull generate controls: fit-parameter drag rows plus the **Generate
/// `CollisionHull`** button, which swaps to an animated spinner while a
/// generation job is running for this prim
/// ([`InspectorContext::collision_hull_generating`]). Parameters persist
/// per-prim in egui temp memory, so a prior configuration survives
/// re-selection. Conductor-only (callers gate on `editable`).
fn draw_hull_generate(ui: &mut Ui, ctx: &mut InspectorContext<'_>) {
    ui.separator();
    let params_id = egui::Id::new(("idtx_collision_hull_params", ctx.info.path.as_str()));
    let mut params: HullGenParams = ui
        .data_mut(|d| d.get_temp::<HullGenParams>(params_id))
        .unwrap_or_default();

    ui.horizontal(|ui| {
        ui.label("max hull vertices");
        ui.add(
            egui::DragValue::new(&mut params.max_hull_vertices)
                .speed(1.0)
                .range(4u32..=HULL_MAX_VERTICES),
        )
        .on_hover_text(
            "Cap on the generated hull's vertex count (the collision detail \
             knob). Fewer vertices → a stabler resting contact (a dense hull \
             makes a dynamic body jitter and never sleep) and cheaper collision. \
             The physics simulation uses the generated hull verbatim.",
        );
    });
    ui.horizontal(|ui| {
        ui.label("max input points");
        ui.add(
            egui::DragValue::new(&mut params.max_input_points)
                .speed(100.0)
                .range(16u32..=10_000_000),
        );
    });
    ui.data_mut(|d| d.insert_temp(params_id, params));

    if ctx.collision_hull_generating {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new());
            ui.label("Generating hull…");
        });
    } else if ui.button("Generate CollisionHull").clicked() {
        ctx.generate_collision_hull(params);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        channel_is_set, collision_section, derived_analytic_label, offers_body_marker,
        body_marker_interactive, parse_enabled, CollisionSection,
    };

    #[test]
    fn derived_shape_maps_gprim_types() {
        assert_eq!(derived_analytic_label("Sphere"), "sphere");
        assert_eq!(derived_analytic_label("Cube"), "cube");
        assert_eq!(derived_analytic_label("Cylinder"), "cylinder");
        assert_eq!(derived_analytic_label("Cone"), "cone");
        assert_eq!(derived_analytic_label("Capsule"), "capsule");
        // Anything else collides against its mesh.
        assert_eq!(derived_analytic_label("Xform"), "mesh");
        assert_eq!(derived_analytic_label(""), "mesh");
    }

    #[test]
    fn section_present_when_collider_applied_regardless_of_product() {
        // Applied → shown in every product; editable only in conductor.
        assert_eq!(
            collision_section(true, true, true),
            CollisionSection::Present { editable: true }
        );
        assert_eq!(
            collision_section(true, false, false),
            CollisionSection::Present { editable: false }
        );
    }

    #[test]
    fn section_offers_add_only_for_xformable_in_conductor() {
        assert_eq!(
            collision_section(false, true, true),
            CollisionSection::OfferAdd
        );
        // Not conductor-editable → no "Add Collision".
        assert_eq!(
            collision_section(false, true, false),
            CollisionSection::Hidden
        );
        // Not xformable → nothing to offer.
        assert_eq!(
            collision_section(false, false, true),
            CollisionSection::Hidden
        );
    }

    #[test]
    fn channel_membership_parses_summaries() {
        assert!(channel_is_set("[Physics, Interaction]", "Physics"));
        assert!(channel_is_set("[\"All\"]", "All"));
        assert!(!channel_is_set("[Physics]", "Interaction"));
        // "All" must not match as a substring of another word.
        assert!(!channel_is_set("[Alliance]", "All"));
    }

    #[test]
    fn enabled_defaults_true_and_parses_false() {
        assert!(parse_enabled("true"));
        assert!(parse_enabled("1"));
        assert!(parse_enabled(""));
        assert!(!parse_enabled("false"));
        assert!(!parse_enabled("0"));
    }

    #[test]
    fn body_marker_offered_for_xformable_or_gprim_only_in_conductor() {
        // Grouping Xformable in conductor → offered.
        assert!(offers_body_marker(true, false, true));
        // Collider gprim in conductor → offered.
        assert!(offers_body_marker(false, true, true));
        // Not conductor-editable → never offered.
        assert!(!offers_body_marker(true, true, false));
        // Neither Xformable nor gprim → nothing to mark.
        assert!(!offers_body_marker(false, false, true));
    }

    #[test]
    fn plane_body_markers_are_disabled_not_active() {
        // A Plane is a gprim, so a body marker is geometrically *offered* …
        assert!(offers_body_marker(false, true, true));
        // … but it must never be interactable (shown disabled with a hover-why).
        assert!(!body_marker_interactive(true));
        // Any other gprim/Xformable keeps its active marker.
        assert!(body_marker_interactive(false));
    }

    #[test]
    fn derived_shape_labels_plane_as_static_half_space() {
        assert_eq!(derived_analytic_label("Plane"), "plane (static half-space)");
    }
}
