//! Session-side implementation of [`idtx_behavior::SceneBackend`].
//!
//! The behavior host is USD-free, so the raycast / arc-cast / prim-type-query
//! imports delegate to this type, which lives in the session (where
//! `idtx-scene` picking + the hydrator world are available). Installed on the
//! [`BehaviorHost`](idtx_behavior::BehaviorHost) via `set_scene_backend`.
//!
//! ## Coordinate space
//!
//! A behavior supplies rays/arcs in **stage space** — the frame its XR aim/HMD
//! poses (`idtx_xr_get_pose`) are reported in, which is the same frame the
//! viewport picker calls "render world space". The ECS `WorldTransform` /
//! `BoundsComp` geometry is in **USD world space**. So, exactly like
//! [`StudioSession::pick_and_resolve`](crate::StudioSession::pick_and_resolve),
//! this backend maps the incoming ray stage→USD with `projection_basis_inv`
//! before picking, and maps the hit position USD→stage with `projection_basis`
//! before returning it. Distances are reported in stage units.

use bevy_ecs::entity::Entity as EcsEntity;
use bevy_ecs::world::World;
use glam::{Mat4, Vec3};
use idtx_behavior::{SceneBackend, WidgetHitInfo};
use idtx_behavior_abi::{
    self as abi, ArcCastParams, ArcHitResult, Entity as WireEntity, RaycastParams, RayHitResult,
};
use idtx_scene::PrimNode;

fn curve_error(error: idtx_usd::curve::CurveError) -> abi::ErrorCode {
    match error {
        idtx_usd::curve::CurveError::Unsupported(_) => abi::ErrorCode::TypeMismatch,
        _ => abi::ErrorCode::InvalidArgument,
    }
}

fn curve_component(world: &World, entity: EcsEntity) -> Result<&idtx_scene::CurveComp, abi::ErrorCode> {
    world.get::<idtx_scene::CurveComp>(entity).ok_or_else(|| {
        if world.get::<PrimNode>(entity).is_some_and(|n| matches!(n.type_name.as_str(), "NurbsCurves" | "HermiteCurves")) { abi::ErrorCode::TypeMismatch } else { abi::ErrorCode::NotFound }
    })
}
use idtx_scene::picking::{RayFilter, pick_prim_filtered, type_name_hash};
use idtx_scene::{
    VisibilityComp, WidgetPanelComp, WidgetSpace, WorldTransform,
};
#[cfg(test)]
use idtx_scene::WidgetBillboard;

/// Stateless scene-query backend bound to a projection basis.
///
/// Recreated and re-installed whenever the basis changes (stage units / up
/// axis) or the host is rebuilt — see `StudioSession::install_scene_backend`.
#[derive(Debug, Clone, Copy)]
pub struct StudioSceneBackend {
    /// USD world space → stage/render space.
    basis: Mat4,
    /// stage/render space → USD world space.
    basis_inv: Mat4,
}

impl StudioSceneBackend {
    /// Build a backend for the given projection basis + its inverse.
    #[must_use]
    pub fn new(basis: Mat4, basis_inv: Mat4) -> Self {
        Self { basis, basis_inv }
    }

    /// Map a stage-space ray to a normalized USD-space ray. Returns `None` when
    /// the direction degenerates.
    fn usd_ray(&self, origin: Vec3, dir: Vec3) -> Option<(Vec3, Vec3)> {
        let o = self.basis_inv.transform_point3(origin);
        let d = self.basis_inv.transform_vector3(dir).normalize_or_zero();
        (d != Vec3::ZERO).then_some((o, d))
    }
}

/// Translate wire raycast params into a scene [`RayFilter`]. `max_distance` is
/// deliberately left unbounded here (stage↔USD scale differs); the backend
/// enforces the stage-space limit on the resolved hit.
fn ray_filter(flags: u32, type_filter_hash: u64, purpose_mask: u32, channel_mask: u32) -> RayFilter {
    RayFilter {
        require_collidable: flags & abi::RAYCAST_REQUIRE_COLLIDABLE != 0,
        respect_visibility: flags & abi::RAYCAST_RESPECT_VISIBILITY != 0,
        type_filter_hash,
        type_exclude: flags & abi::RAYCAST_TYPE_EXCLUDE != 0,
        max_distance: 0.0,
        purpose_mask,
        channel_mask,
    }
}

fn to_wire(e: EcsEntity) -> WireEntity {
    WireEntity::from_bits(e.to_bits())
}

/// Plane-intersection epsilon (ray near-parallel to the panel).
const WIDGET_PLANE_EPS: f32 = 1e-6;

/// USD-space quad geometry of a panel: `(center, a, b)` where `a`/`b` are the
/// half-edge vectors along panel-local +X / +Y. Matches `idtx_render`'s
/// `resolve_quad` for `billboard = none` so the hit-test lands on the same
/// element the rasterizer drew.
#[cfg(test)]
fn panel_quad(matrix: Mat4, size: [f32; 2]) -> (Vec3, Vec3, Vec3) {
    idtx_scene::widget_layout::resolve_widget_quad(matrix, size, WidgetBillboard::None, Vec3::X, Vec3::Y)
}

fn interaction_quad(world: &World, backend: &StudioSceneBackend, panel: &WidgetPanelComp, matrix: Mat4) -> (Vec3, Vec3, Vec3) {
    let camera = world.get_resource::<idtx_scene::widget_layout::WidgetInteractionCamera>()
        .map_or(Mat4::IDENTITY, |c| c.0);
    let (c, a, b) = idtx_scene::widget_layout::resolve_widget_quad(backend.basis * matrix,
        panel.size, panel.billboard, camera.x_axis.truncate(), camera.y_axis.truncate());
    (backend.basis_inv.transform_point3(c), backend.basis_inv.transform_vector3(a), backend.basis_inv.transform_vector3(b))
}

/// Panel-local UV (top-left origin) of a world-space point already on the panel
/// plane. `+Y` (up) maps to the top of the image, matching the render V-flip.
///
/// The renderer draws the panel as the parallelogram spanned by the half-edge
/// vectors `a`/`b` (which carry the full world transform, so they need not be
/// orthogonal under shear/non-uniform scale). The panel-local coordinates of a
/// point are therefore the `(s, t)` that solve `rel = s·a + t·b`, obtained from
/// the 2×2 Gram system — *not* independent axis projections, which are only
/// correct when `a ⟂ b` and would otherwise paint one element while dispatching
/// clicks to another. Returns `NaN`s for a degenerate (collinear) basis so the
/// caller's `0..=1` range check rejects the hit.
fn point_panel_uv(center: Vec3, a: Vec3, b: Vec3, p: Vec3) -> [f32; 2] {
    let rel = p - center;
    let aa = a.dot(a);
    let ab = a.dot(b);
    let bb = b.dot(b);
    let det = aa * bb - ab * ab;
    if det.abs() < WIDGET_PLANE_EPS {
        return [f32::NAN; 2];
    }
    let ra = rel.dot(a);
    let rb = rel.dot(b);
    let s = (ra * bb - rb * ab) / det;
    let t = (rb * aa - ra * ab) / det;
    [s * 0.5 + 0.5, 0.5 - t * 0.5]
}

/// Intersect ray `(o, d)` (d normalized) with the panel quad. Returns `(t, uv)`
/// when the ray crosses inside the quad; `uv` is top-left origin. `t` is the USD
/// distance along `d`.
///
/// `double_sided` mirrors the renderer's face culling: a single-sided panel
/// culls its back face (`idtx:widget:doubleSided = false`), so a ray arriving
/// from behind must be rejected here too — otherwise an invisible back face
/// would still block rays and receive clicks. The panel's front normal is
/// `a × b` (matching the render quad winding); a front hit has `d · n < 0`.
fn ray_panel(
    o: Vec3,
    d: Vec3,
    center: Vec3,
    a: Vec3,
    b: Vec3,
    double_sided: bool,
) -> Option<(f32, [f32; 2])> {
    let n = a.cross(b);
    let denom = d.dot(n);
    if denom.abs() < WIDGET_PLANE_EPS {
        return None;
    }
    // Single-sided panels are only hit from the front face (ray opposing the
    // normal); a back-facing ray is culled by the renderer and rejected here.
    if !double_sided && denom > 0.0 {
        return None;
    }
    let t = (center - o).dot(n) / denom;
    if t <= 0.0 {
        return None;
    }
    let p = o + d * t;
    let uv = point_panel_uv(center, a, b, p);
    if (0.0..=1.0).contains(&uv[0]) && (0.0..=1.0).contains(&uv[1]) {
        Some((t, uv))
    } else {
        None
    }
}

/// Whether a panel is eligible for interaction / blocking: visible, in-world
/// (not HUD), and
/// not fully transparent. **Invisible panels are never hit-tested.**
fn panel_blockable(panel: &WidgetPanelComp, vis: Option<&VisibilityComp>) -> bool {
    vis.is_none_or(|v| v.visible)
        && panel.space == WidgetSpace::World
        && panel.opacity.is_finite() && panel.opacity > 0.0
}

/// Nearest eligible widget panel the USD-space ray `(o, d)` crosses, as
/// `(panel entity, t)`. Iterates only blockable panels (see
/// [`panel_blockable`]).
fn nearest_widget_hit(world: &mut World, backend: &StudioSceneBackend, o: Vec3, d: Vec3) -> Option<(EcsEntity, f32)> {
    let mut best: Option<(EcsEntity, f32)> = None;
    let mut q =
        world.query::<(EcsEntity, &WidgetPanelComp, &WorldTransform, Option<&VisibilityComp>)>();
    for (e, panel, xf, vis) in q.iter(world) {
        if !panel_blockable(panel, vis) {
            continue;
        }
        let (center, a, b) = interaction_quad(world, backend, panel, xf.matrix);
        if let Some((t, uv)) = ray_panel(o, d, center, a, b, panel.double_sided) {
            if !idtx_scene::widget_layout::widget_surface_hit(panel.resolution, &panel.elements, &panel.surface, uv) { continue; }
            if best.is_none_or(|(_, bt)| t < bt) {
                best = Some((e, t));
            }
        }
    }
    best
}

impl SceneBackend for StudioSceneBackend {
    fn input_player(&self, world: &World, entity: EcsEntity) -> Option<EcsEntity> {
        idtx_scene::nearest_authored_player_root(world, entity).map(|(e, _)| e)
    }

    fn input_target_available(&self, world: &World, entity: EcsEntity) -> bool {
        world.get::<WidgetPanelComp>(entity).map_or_else(
            || world.get::<VisibilityComp>(entity).is_none_or(|v| v.visible),
            |p| panel_blockable(p, world.get::<VisibilityComp>(entity)))
    }

    fn widget_element(&self, world: &World, entity: EcsEntity, path: &str) -> Option<WidgetHitInfo> {
        use idtx_scene::widget_layout::*;
        let panel = world.get::<WidgetPanelComp>(entity)?;
        if !panel_blockable(panel, world.get::<VisibilityComp>(entity)) { return None; }
        let index = panel.elements.iter().position(|e| e.path == path && e.enabled && e.opacity > 0.0
            && e.kind != idtx_scene::WidgetElementKind::Container)?;
        let layout = compute_widget_layout(panel.resolution, &panel.elements);
        let item = layout.iter().find(|e| e.index == index)?;
        // A fully clipped element is not activatable. Use a covered sample,
        // including paint-order validation, rather than assuming its label center.
        let to_uv = |p: [f32; 2]| [p[0] / panel.resolution[0].max(1) as f32, p[1] / panel.resolution[1].max(1) as f32];
        let visible = |p: [f32; 2]| item.contains(p) && panel.surface.contains(to_uv(p))
            && layout.iter().rev().find(|r| r.contains(p)).is_some_and(|r| r.index == index);
        let point = if visible(item.center) { item.center } else {
            (item.clip.y0..item.clip.y1).find_map(|y| (item.clip.x0..item.clip.x1)
                .map(|x| [x as f32 + 0.5, y as f32 + 0.5]).find(|&p| visible(p)))?
        };
        let uv = to_uv(point);
        let xf = world.get::<WorldTransform>(entity)?;
        let (center, a, b) = interaction_quad(world, self, panel, xf.matrix);
        Some(WidgetHitInfo { element: Some(path.into()), uv,
            position: self.basis.transform_point3(center + a * (uv[0] * 2.0 - 1.0) + b * (1.0 - uv[1] * 2.0)),
            normal: self.basis.transform_vector3(a.cross(b)).normalize_or_zero() })
    }

    fn widget_navigation(&self, world: &World, entity: EcsEntity, container: &str,
        previous: &str, stick: [f32; 2], step: i32) -> Option<String> {
        use idtx_scene::widget_layout::*;
        let panel = world.get::<WidgetPanelComp>(entity)?;
        if !panel_blockable(panel, world.get::<VisibilityComp>(entity)) { return None; }
        let layout = compute_widget_layout(panel.resolution, &panel.elements);
        let previous = panel.elements.iter().position(|e| e.path == previous);
        let index = if step == 0 { radial_stick_target(&layout, &panel.elements, container, stick, previous)? }
        else {
            let candidates: Vec<_> = layout.iter().filter(|r| {
                let e = &panel.elements[r.index];
                e.enabled && e.kind == idtx_scene::WidgetElementKind::Button
                    && e.path.rsplit_once('/').map_or("", |(p, _)| p) == container
            }).map(|r| r.index).collect();
            if candidates.is_empty() { return None; }
            let selected = previous.and_then(|p| candidates.iter().position(|&i| i == p));
            let at = selected.map_or(if step > 0 { 0 } else { candidates.len() - 1 }, |i|
                (i as isize + step.signum() as isize).rem_euclid(candidates.len() as isize) as usize);
            candidates[at]
        };
        let path = &panel.elements[index].path;
        self.widget_element(world, entity, path).map(|_| path.clone())
    }

    fn widget_event_valid(&self, world: &World, entity: EcsEntity, element: Option<&str>) -> bool {
        match element {
            Some(path) => self.widget_element(world, entity, path).is_some(),
            None => world.get::<WidgetPanelComp>(entity).is_some_and(|p|
                panel_blockable(p, world.get::<VisibilityComp>(entity))),
        }
    }
    fn curve_length(&self, world: &World, entity: EcsEntity, index: u32) -> Result<f64, abi::ErrorCode> {
        let curve = curve_component(world, entity)?;
        Ok(curve.curve(index as usize).map_err(curve_error)?.length())
    }

    fn curve_samples(&self, world: &World, entity: EcsEntity, index: u32, space: u32, distances: &[f64], out: &mut [abi::CurveSample]) -> Result<(), abi::ErrorCode> {
        if distances.len() > abi::MAX_CURVE_QUERY_SAMPLES || out.len() < distances.len() || space > 1 || distances.iter().any(|x| !x.is_finite()) { return Err(abi::ErrorCode::InvalidArgument); }
        let curve = curve_component(world, entity)?.curve(index as usize).map_err(curve_error)?;
        let space = if space == 0 { idtx_usd::curve::CurveSpace::Local } else { idtx_usd::curve::CurveSpace::StageWorldMetres };
        for (&distance, out) in distances.iter().zip(out) {
            let sample = curve.sample_distance(distance, space).map_err(curve_error)?;
            *out = abi::CurveSample { point: sample.point.to_array(), tangent: sample.tangent.to_array() };
        }
        Ok(())
    }
    fn raycast(&self, world: &mut World, params: &RaycastParams) -> RayHitResult {
        let origin_stage = Vec3::from(params.origin);
        let Some((usd_o, usd_d)) = self.usd_ray(origin_stage, Vec3::from(params.dir)) else {
            return RayHitResult::default();
        };
        let filter = ray_filter(
            params.flags,
            params.type_filter_hash,
            params.purpose_mask,
            params.channel_mask,
        );
        // Geometry hit (USD distance `t` along `usd_d`).
        let geom = pick_prim_filtered(world, usd_o, usd_d, &filter);
        // Widget-panel hit, only when the cast opted in.
        let widget = (params.flags & abi::RAYCAST_BLOCK_WIDGETS != 0)
            .then(|| nearest_widget_hit(world, self, usd_o, usd_d))
            .flatten();

        // Nearest of the two wins; a widget-nearest result is flagged.
        let (entity, t_usd, is_widget) = match (geom, widget) {
            (Some(g), Some(w)) => {
                if w.1 < g.t {
                    (w.0, w.1, true)
                } else {
                    (g.entity, g.t, false)
                }
            }
            (Some(g), None) => (g.entity, g.t, false),
            (None, Some(w)) => (w.0, w.1, true),
            (None, None) => return RayHitResult::default(),
        };

        let stage_point = self.basis.transform_point3(usd_o + t_usd * usd_d);
        let distance = (stage_point - origin_stage).length();
        if params.max_distance > 0.0 && distance > params.max_distance {
            return RayHitResult::default();
        }
        RayHitResult {
            entity: to_wire(entity),
            position: stage_point.into(),
            distance,
            hit: 1,
            flags: if is_widget { abi::RAYHIT_WIDGET } else { 0 },
        }
    }

    #[allow(clippy::cast_precision_loss)] // arc sample index → f32 is exact for small N
    fn arc_cast(
        &self,
        world: &mut World,
        params: &ArcCastParams,
        out_points: &mut [glam::Vec3],
    ) -> ArcHitResult {
        let origin = Vec3::from(params.origin);
        let dir = Vec3::from(params.dir).normalize_or_zero();
        // Sample the parabola in stage space, where speed/gravity are defined:
        // p(t) = origin + dir·speed·t − ½·g·t²·ŷ, world +Y up.
        let samples = (params.sample_count.max(2) as usize).min(out_points.len().max(2));
        let up = Vec3::Y;
        let max_t = params.max_time.max(0.0);
        let point_at = |t: f32| origin + dir * params.speed * t - 0.5 * params.gravity * t * t * up;

        // `ArcCastParams` carries no channel mask (deferred — adding one forces
        // an `ABI_VERSION` bump), so arc-casts are always channel-unfiltered.
        let filter = ray_filter(
            params.flags,
            params.type_filter_hash,
            params.purpose_mask,
            abi::CHANNEL_ANY,
        );
        let mut result = ArcHitResult {
            sample_count: u32::try_from(samples).unwrap_or(u32::MAX),
            ..ArcHitResult::default()
        };

        let mut prev_stage = origin;
        for i in 0..samples {
            let t = if samples <= 1 {
                0.0
            } else {
                max_t * (i as f32) / ((samples - 1) as f32)
            };
            let p = point_at(t);
            if let Some(slot) = out_points.get_mut(i) {
                *slot = p;
            }
            if i == 0 {
                prev_stage = p;
                continue;
            }
            // Raycast the segment [prev_stage, p] (USD space, bounded by its
            // length) and stop at the first hit.
            if result.hit == 0
                && let Some((seg_o, seg_d)) = self.usd_ray(prev_stage, p - prev_stage) {
                    let seg_len_usd = self.basis_inv.transform_vector3(p - prev_stage).length();
                    let mut seg_filter = filter;
                    seg_filter.max_distance = seg_len_usd;
                    if let Some(hit) = pick_prim_filtered(world, seg_o, seg_d, &seg_filter) {
                        let stage_point = self.basis.transform_point3(seg_o + hit.t * seg_d);
                        result.hit = 1;
                        result.hit_entity = to_wire(hit.entity);
                        result.hit_position = stage_point.into();
                        result.hit_distance = (stage_point - origin).length();
                    }
                }
            prev_stage = p;
        }
        result
    }

    fn find_by_type(&self, world: &mut World, type_hash: u64, out: &mut Vec<EcsEntity>) {
        out.clear();
        let mut q = world.query::<(EcsEntity, &PrimNode)>();
        for (e, node) in q.iter(world) {
            if type_name_hash(&node.type_name) == type_hash {
                out.push(e);
            }
        }
    }

    fn path_of(&self, world: &World, entity: EcsEntity) -> Option<String> {
        world.get::<PrimNode>(entity).map(|n| n.path.clone())
    }

    fn world_xform(&self, world: &World, entity: EcsEntity) -> Option<abi::Xform> {
        // Hydrated prims carry `idtx-scene`'s `WorldTransform` (USD world space);
        // map it into stage space the same way the viewport does
        // (`projection_basis * worldMatrix`), then decompose for the wire form.
        let world_matrix = world.get::<idtx_scene::WorldTransform>(entity)?.matrix;
        let (scale, rotation, translation) =
            (self.basis * world_matrix).to_scale_rotation_translation();
        Some(abi::Xform::from_trs(translation, rotation, scale))
    }

    fn find_by_path(&self, world: &mut World, path: &str) -> Option<EcsEntity> {
        let mut q = world.query::<(EcsEntity, &PrimNode)>();
        q.iter(world).find(|(_, node)| node.path == path).map(|(e, _)| e)
    }

    fn parent(&self, world: &mut World, entity: EcsEntity) -> Option<EcsEntity> {
        // Derive the parent path by stripping the leaf segment, then resolve it.
        // (Avoids needing a PrimId→Entity reverse index.) The pseudo-root (`/`,
        // or a top-level prim whose parent is `/`) has no behavior-visible parent.
        let path = world.get::<PrimNode>(entity)?.path.clone();
        let (parent_path, _leaf) = path.rsplit_once('/')?;
        if parent_path.is_empty() {
            return None; // top-level prim: parent is the pseudo-root `/`
        }
        self.find_by_path(world, parent_path)
    }

    fn collision_body(&self, world: &World, entity: EcsEntity) -> Option<EcsEntity> {
        idtx_scene::collision_body_of(world, entity)
    }

    fn visibility(&self, world: &World, entity: EcsEntity) -> Option<bool> {
        // The renderer draws from `VisibilityComp.visible` — the effective
        // visibility the hydrator recomputes from the `VisibilityOverride`
        // chain — so that is exactly what a behavior reads back.
        Some(world.get::<VisibilityComp>(entity)?.visible)
    }

    fn physics_body(&self, world: &World, entity: EcsEntity) -> Option<EcsEntity> {
        idtx_scene::rigid_body_of(world, entity)
    }

    fn widget_hit(
        &self,
        world: &World,
        entity: EcsEntity,
        position: Vec3,
    ) -> Option<WidgetHitInfo> {
        let panel = world.get::<WidgetPanelComp>(entity)?;
        let xf = world.get::<WorldTransform>(entity)?;
        let vis = world.get::<VisibilityComp>(entity);
        if !panel_blockable(panel, vis) {
            return None;
        }
        let (center, a, b) = interaction_quad(world, self, panel, xf.matrix);
        // Map the stage-space hit back to USD space (the panel matrix frame) to
        // read the panel-local UV.
        let usd_pos = self.basis_inv.transform_point3(position);
        let uv = point_panel_uv(center, a, b, usd_pos);
        if !idtx_scene::widget_layout::widget_surface_hit(panel.resolution, &panel.elements, &panel.surface, uv) { return None; }
        // Resolve the UV to a child element (shared layout with the rasterizer);
        // a disabled element produces no dispatchable path (cue still shows).
        let element =
            idtx_render::widget_hit_test(panel.resolution, &panel.elements, uv[0], uv[1])
                .and_then(|idx| {
                    let el = &panel.elements[idx];
                    el.enabled.then(|| el.path.clone())
                });
        // Panel normal (USD → stage) for the hit-point cue orientation.
        let normal = self.basis.transform_vector3(a.cross(b)).normalize_or_zero();
        Some(WidgetHitInfo {
            element,
            uv,
            position,
            normal,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use idtx_scene::WorldTransform;
    use idtx_usd::{PrimId, PrimState, StageGeneration};

    fn prim(slot: u32, path: &str) -> PrimNode {
        PrimNode {
            prim: PrimId::from_raw_parts(slot, StageGeneration::from_raw(1)),
            path: path.to_owned(),
            parent: None,
            name: path.rsplit('/').next().unwrap_or_default().to_owned(),
            type_name: "Xform".to_owned(),
            state: PrimState {
                active: true,
                defined: true,
                loaded: true,
            },
            is_pseudo_root: false,
            local_layer_identifier: None,
        }
    }

    fn identity_backend() -> StudioSceneBackend {
        StudioSceneBackend::new(Mat4::IDENTITY, Mat4::IDENTITY)
    }

    #[test]
    fn widget_draw_pick_parity_for_billboards_parents_and_polygon_corners() {
        use idtx_scene::widget_layout::{WidgetInteractionCamera, WidgetPolygon, WidgetSurface, resolve_widget_quad};
        let backend = identity_backend();
        let camera = Mat4::from_quat(glam::Quat::from_rotation_y(0.7));
        let transform = Mat4::from_scale_rotation_translation(Vec3::new(2.0, 1.5, 1.0),
            glam::Quat::from_rotation_z(0.4) * glam::Quat::from_rotation_y(-0.3), Vec3::new(3.0, 2.0, -5.0));
        for billboard in WidgetBillboard::ALL {
            let mut world = World::new();
            world.insert_resource(WidgetInteractionCamera(camera));
            let panel = WidgetPanelComp { size: [1.0; 2], resolution: [100; 2], pixels_per_unit: 100.0,
                billboard, double_sided: true, opacity: 1.0, space: WidgetSpace::World,
                content_source: None, surface: WidgetSurface::default(), elements: vec![], content_hash: 1 };
            let entity = world.spawn((panel, WorldTransform { matrix: transform })).id();
            let (center, a, b) = resolve_widget_quad(transform, [1.0; 2], billboard, camera.x_axis.truncate(), camera.y_axis.truncate());
            let n = a.cross(b).normalize();
            let cast = |point: Vec3| RaycastParams { origin: (point + n * 2.0).into(), dir: (-n).into(),
                flags: abi::RAYCAST_BLOCK_WIDGETS, ..Default::default() };
            let hit = backend.raycast(&mut world, &cast(center));
            assert_eq!(hit.entity, to_wire(entity), "blank backing still occludes");
            let info = backend.widget_hit(&world, entity, center).unwrap();
            assert!((info.uv[0] - 0.5).abs() < 1e-5 && (info.uv[1] - 0.5).abs() < 1e-5);
            let mut panel = world.get_mut::<WidgetPanelComp>(entity).unwrap();
            panel.surface.outline = Some(WidgetPolygon::new(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]], vec![0, 1, 2]).unwrap());
            let outside = center + a * 0.6 - b * 0.6; // uv=(.8,.8)
            assert_eq!(backend.raycast(&mut world, &cast(outside)).hit, 0);
            assert!(backend.widget_hit(&world, entity, outside).is_none());
        }
    }

    #[test]
    fn direct_activation_rejects_disabled_hidden_removed_and_missing_children() {
        let mut world = World::new();
        let entity = world.spawn((WidgetPanelComp { size: [1.0; 2], resolution: [100; 2], pixels_per_unit: 100.0,
            billboard: WidgetBillboard::None, double_sided: true, opacity: 1.0, space: WidgetSpace::World,
            content_source: None, surface: Default::default(), elements: vec![idtx_scene::WidgetElement {
                path: "Button".into(), kind: idtx_scene::WidgetElementKind::Button, ..Default::default()
            }], content_hash: 1 }, WorldTransform { matrix: Mat4::IDENTITY })).id();
        let backend = identity_backend();
        assert!(backend.widget_element(&world, entity, "Button").is_some());
        assert!(backend.widget_element(&world, entity, "Missing").is_none());
        world.get_mut::<WidgetPanelComp>(entity).unwrap().elements[0].enabled = false;
        assert!(backend.widget_element(&world, entity, "Button").is_none());
        world.get_mut::<WidgetPanelComp>(entity).unwrap().elements[0].enabled = true;
        world.get_mut::<WidgetPanelComp>(entity).unwrap().opacity = 0.0;
        assert!(backend.widget_element(&world, entity, "Button").is_none());
        world.despawn(entity);
        assert!(backend.widget_element(&world, entity, "Button").is_none());
    }

    #[test]
    fn curve_queries_use_snapshot_metres_and_reject_removed_entities() {
        let path=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/usd/curves/references.usda");
        let mut stage=idtx_usd::StageService::open(path).unwrap();
        let projection=stage.project_viewport().unwrap();
        let record=projection.curves.into_iter().find(|r|stage.path_for(r.prim)==Some("/A/Path")).unwrap();
        let mut world=World::new();
        let entity=world.spawn(idtx_scene::CurveComp::new(record,None,1)).id();
        let basis=Mat4::from_scale(Vec3::splat(100.));
        let backend=StudioSceneBackend::new(basis,basis.inverse());
        assert_eq!(backend.curve_length(&world,entity,0).unwrap(),2.,"curve contract is metres, independent of render unit");
        let mut samples=[abi::CurveSample::default();2];
        backend.curve_samples(&world,entity,0,1,&[0.,1.],&mut samples).unwrap();
        assert!(glam::DVec3::from_array(samples[1].point).abs_diff_eq(glam::DVec3::X,1e-12));
        backend.curve_samples(&world,entity,0,0,&[0.,1.],&mut samples).unwrap();
        assert!((samples[1].point[0]-50.).abs()<1e-9);
        assert!(backend.curve_samples(&world,entity,0,2,&[1.],&mut samples).is_err());
        world.despawn(entity);
        assert!(backend.curve_length(&world,entity,0).is_err());
    }

    #[test]
    fn world_xform_maps_through_basis() {
        let mut world = World::new();
        let e = world
            .spawn((
                prim(1, "/World/Player"),
                WorldTransform {
                    matrix: Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0)),
                },
            ))
            .id();

        // Identity basis: stage world == USD world.
        let got = identity_backend().world_xform(&world, e).unwrap();
        assert!((Vec3::from(got.translation) - Vec3::new(1.0, 2.0, 3.0)).length() < 1e-5);

        // A non-identity basis (uniform 2×) must be applied.
        let basis = Mat4::from_scale(Vec3::splat(2.0));
        let got = StudioSceneBackend::new(basis, basis.inverse())
            .world_xform(&world, e)
            .unwrap();
        assert!((Vec3::from(got.translation) - Vec3::new(2.0, 4.0, 6.0)).length() < 1e-5);
    }

    #[test]
    fn world_xform_none_without_world_transform() {
        let mut world = World::new();
        let e = world.spawn(prim(1, "/World/Player")).id();
        assert!(identity_backend().world_xform(&world, e).is_none());
    }

    #[test]
    fn visibility_reads_visibility_comp() {
        use idtx_scene::RenderPurpose;
        let mut world = World::new();
        let hidden = world
            .spawn((
                prim(1, "/World/Hidden"),
                VisibilityComp {
                    visible: false,
                    purpose: RenderPurpose::Default,
                    casts_shadow: true,
                },
            ))
            .id();
        let shown = world
            .spawn((
                prim(2, "/World/Shown"),
                VisibilityComp {
                    visible: true,
                    purpose: RenderPurpose::Default,
                    casts_shadow: true,
                },
            ))
            .id();
        // A prim with no `VisibilityComp` has no projected visibility.
        let bare = world.spawn(prim(3, "/World/Bare")).id();

        let backend = identity_backend();
        assert_eq!(backend.visibility(&world, hidden), Some(false));
        assert_eq!(backend.visibility(&world, shown), Some(true));
        assert_eq!(backend.visibility(&world, bare), None);
    }

    #[test]
    fn find_by_path_and_parent_resolve() {
        let mut world = World::new();
        let root = world.spawn(prim(1, "/World")).id();
        let player = world.spawn(prim(2, "/World/Player")).id();
        let backend = identity_backend();

        assert_eq!(
            backend.find_by_path(&mut world, "/World/Player"),
            Some(player)
        );
        assert_eq!(backend.find_by_path(&mut world, "/nope"), None);
        // Parent of /World/Player is /World; a top-level prim has no
        // behavior-visible parent (the pseudo-root).
        assert_eq!(backend.parent(&mut world, player), Some(root));
        assert_eq!(backend.parent(&mut world, root), None);
    }

    // -- widget hit-test geometry (pure math, no USD / GPU) -----------

    #[test]
    fn ray_panel_center_hits_at_uv_center() {
        // Unit panel at the origin in the XY plane (size 2 → half-extents = 1),
        // facing +Z. A ray down -Z through the centre lands at UV (0.5, 0.5).
        let (c, a, b) = panel_quad(Mat4::IDENTITY, [2.0, 2.0]);
        let (t, uv) = ray_panel(Vec3::new(0.0, 0.0, 5.0), Vec3::NEG_Z, c, a, b, true).expect("hit");
        assert!((t - 5.0).abs() < 1e-4);
        assert!((uv[0] - 0.5).abs() < 1e-4 && (uv[1] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn ray_panel_top_right_and_miss() {
        let (c, a, b) = panel_quad(Mat4::IDENTITY, [2.0, 2.0]);
        // Local top-right (+x,+y) → UV near (1, 0) (up maps to image top).
        let (_, uv) =
            ray_panel(Vec3::new(0.9, 0.9, 5.0), Vec3::NEG_Z, c, a, b, true).expect("hit");
        assert!(uv[0] > 0.9 && uv[1] < 0.1, "got {uv:?}");
        // Outside the quad extents → miss.
        assert!(ray_panel(Vec3::new(2.0, 0.0, 5.0), Vec3::NEG_Z, c, a, b, true).is_none());
        // Ray pointing away from the panel → miss (t <= 0).
        assert!(ray_panel(Vec3::new(0.0, 0.0, 5.0), Vec3::Z, c, a, b, true).is_none());
    }

    #[test]
    fn ray_panel_single_sided_rejects_back_face() {
        // Panel in the XY plane: front normal = a × b = +Z. A ray from in front
        // (down -Z) is a front hit; a ray from behind (up +Z through the centre)
        // hits the culled back face and is rejected for a single-sided panel but
        // accepted for a double-sided one.
        let (c, a, b) = panel_quad(Mat4::IDENTITY, [2.0, 2.0]);
        // From the front (+Z side, travelling -Z): hits regardless of sidedness.
        assert!(ray_panel(Vec3::new(0.0, 0.0, 5.0), Vec3::NEG_Z, c, a, b, false).is_some());
        // From behind (-Z side, travelling +Z): back face.
        assert!(
            ray_panel(Vec3::new(0.0, 0.0, -5.0), Vec3::Z, c, a, b, false).is_none(),
            "single-sided panel must not be hit from behind"
        );
        assert!(
            ray_panel(Vec3::new(0.0, 0.0, -5.0), Vec3::Z, c, a, b, true).is_some(),
            "double-sided panel is hittable from either face"
        );
    }

    #[test]
    fn point_uv_sheared_basis_solves_gram_system() {
        // A sheared parallelogram: a = +X, b = X + Y (not orthogonal to a).
        // The corner reached by a (s=1, t=0) must map to UV (1, 0.5) — the
        // independent-projection formula would have leaked `a` into `t`.
        let center = Vec3::ZERO;
        let a = Vec3::X;
        let b = Vec3::new(1.0, 1.0, 0.0);
        let uv = point_panel_uv(center, a, b, a); // point exactly at center + a
        assert!((uv[0] - 1.0).abs() < 1e-4 && (uv[1] - 0.5).abs() < 1e-4, "got {uv:?}");
        // Point at center + b → (s=0, t=1) → UV (0.5, 0.0).
        let uv = point_panel_uv(center, a, b, b);
        assert!((uv[0] - 0.5).abs() < 1e-4 && (uv[1]).abs() < 1e-4, "got {uv:?}");
    }

    #[test]
    fn point_uv_flips_v_for_top_origin() {
        let (c, a, b) = panel_quad(Mat4::IDENTITY, [2.0, 2.0]);
        // Panel-local bottom-left corner (-x, -y) → UV (0, 1).
        let uv = point_panel_uv(c, a, b, Vec3::new(-1.0, -1.0, 0.0));
        assert!((uv[0]).abs() < 1e-4 && (uv[1] - 1.0).abs() < 1e-4, "got {uv:?}");
    }
}
