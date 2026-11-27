//! Picking against the exact filtered render geometry, including instances.
use glam::{Mat4, Vec3};

use super::{RenderWorldSnapshot, purpose::world_positions};

impl RenderWorldSnapshot {
    /// Closest rendered prim along a render-space ray. Guide triangles supply
    /// their wireframe's picking surface; hidden and filtered prims have no
    /// snapshot entry. Point instances select their owning PointInstancer.
    pub fn pick_prim_path(&self, origin: Vec3, direction: Vec3) -> Option<String> {
        self.pick_geometry(origin, direction, true)
            .map(|(path, _)| path)
    }

    /// Closest visible instance placement, supplementing ECS picking of ordinary
    /// geometry and editor curve overlays. Distance is in render-space units.
    pub fn pick_instanced_prim(&self, origin: Vec3, direction: Vec3) -> Option<(String, f32)> {
        self.pick_geometry(origin, direction, false)
    }

    fn pick_geometry(
        &self,
        origin: Vec3,
        direction: Vec3,
        include_meshes: bool,
    ) -> Option<(String, f32)> {
        if !origin.is_finite()
            || !direction.is_finite()
            || direction.length_squared() <= f32::EPSILON
        {
            return None;
        }
        let direction = direction.normalize();
        let mut nearest = f32::INFINITY;
        let mut path = None;
        let mut consider = |mesh: &crate::GpuMeshDesc, matrix: Mat4, candidate: &str| {
            if let Some(distance) = mesh_hit(mesh, matrix, origin, direction) {
                if distance < nearest {
                    nearest = distance;
                    path = Some(candidate.to_owned());
                }
            }
        };
        for instance in self
            .meshes
            .iter()
            .chain(&self.guide_meshes)
            .filter(|_| include_meshes)
        {
            consider(
                &instance.mesh,
                instance.world_matrix,
                &instance.mesh.prim_path,
            );
        }
        for (batches, metadata) in [
            (&self.instanced_batches, &self.point_instancer_batches),
            (
                &self.guide_instanced_batches,
                &self.guide_point_instancer_batches,
            ),
        ] {
            for (index, batch) in batches.iter().enumerate() {
                for (slot, &matrix) in batch.instance_transforms.iter().enumerate() {
                    if let Some(candidate) = metadata
                        .get(&index)
                        .map(|m| m.owner_path.as_str())
                        .or_else(|| batch.instance_prim_paths.get(slot).map(String::as_str))
                    {
                        consider(&batch.mesh, matrix, candidate);
                    }
                }
            }
        }
        path.map(|path| (path, nearest))
    }
}

fn mesh_hit(mesh: &crate::GpuMeshDesc, matrix: Mat4, origin: Vec3, direction: Vec3) -> Option<f32> {
    if mesh.skin.is_none() {
        if let Some((minimum, maximum)) = mesh.local_aabb {
            let inverse = matrix.inverse();
            if inverse.is_finite()
                && !bounds_hit(
                    inverse.transform_point3(origin),
                    inverse.transform_vector3(direction),
                    minimum.into(),
                    maximum.into(),
                )
            {
                return None;
            }
        }
    }
    let points = world_positions(mesh, matrix);
    let mut closest = f32::INFINITY;
    for triangle in mesh.indices.chunks_exact(3) {
        let Some(a) = points.get(triangle[0] as usize) else {
            continue;
        };
        let Some(b) = points.get(triangle[1] as usize) else {
            continue;
        };
        let Some(c) = points.get(triangle[2] as usize) else {
            continue;
        };
        if let Some(distance) =
            triangle_hit(origin, direction, (*a).into(), (*b).into(), (*c).into())
        {
            closest = closest.min(distance);
        }
    }
    closest.is_finite().then_some(closest)
}

fn bounds_hit(origin: Vec3, direction: Vec3, minimum: Vec3, maximum: Vec3) -> bool {
    let mut enter = 0.0_f32;
    let mut exit = f32::INFINITY;
    for axis in 0..3 {
        if direction[axis].abs() < 1e-12 {
            if origin[axis] < minimum[axis] || origin[axis] > maximum[axis] {
                return false;
            }
        } else {
            let a = (minimum[axis] - origin[axis]) / direction[axis];
            let b = (maximum[axis] - origin[axis]) / direction[axis];
            enter = enter.max(a.min(b));
            exit = exit.min(a.max(b));
            if exit < enter {
                return false;
            }
        }
    }
    true
}

fn triangle_hit(origin: Vec3, direction: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let ab = b - a;
    let ac = c - a;
    let h = direction.cross(ac);
    let determinant = ab.dot(h);
    if determinant.abs() < 1e-8 {
        return None;
    }
    let inverse = determinant.recip();
    let s = origin - a;
    let u = inverse * s.dot(h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(ab);
    let v = inverse * direction.dot(q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let distance = inverse * ac.dot(q);
    (distance >= 0.0).then_some(distance)
}
