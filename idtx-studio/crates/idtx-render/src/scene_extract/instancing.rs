//! Topology-aware pseudo-instance promotion and the geometry fingerprint used
//! to validate that grouped prims differ only in their transforms.

use std::collections::{HashMap, HashSet};

use glam::Mat4;
use idtx_usd::RenderPurpose;

use crate::mesh::GpuMeshDesc;

use super::mesh_instance::RenderInstancedBatch;
use super::snapshot::RenderWorldSnapshot;

/// Hashes mesh geometry for pseudo-instance deduplication.
///
/// Samples vertex count, index count, 5 evenly-spaced positions, and the first
/// 8 index values.  Probabilistic — used as a fast pre-filter before topology
/// or material checks.
pub(crate) fn geometry_fingerprint(m: &GpuMeshDesc) -> u64 {
    let n = m.positions.len();
    let ni = m.indices.len();
    let mut h =
        (n as u64).wrapping_mul(0x9e3779b97f4a7c15) ^ (ni as u64).wrapping_mul(0x6c62272e07bb0142);
    for frac in &[0usize, 1, 2, 3, 4] {
        let i = if n == 0 {
            0
        } else {
            (*frac * n / 5).min(n - 1)
        };
        if let Some(p) = m.positions.get(i) {
            h ^= u64::from(p[0].to_bits());
            h = h.rotate_left(13);
            h ^= u64::from(p[1].to_bits());
            h = h.rotate_left(13);
            h ^= u64::from(p[2].to_bits());
            h = h.rotate_left(13);
        }
    }
    for &idx in m.indices.iter().take(8) {
        h ^= u64::from(idx);
        h = h.rotate_left(7);
    }
    h
}

/// Implements the topology-aware pseudo-instance promotion given pre-computed instance groups.
///
/// `groups` maps prototype `SdfPath` → list of instance-root `SdfPaths`.  This split allows unit
/// tests to call the promotion logic directly without needing a live ECS `World`.
pub(crate) fn promote_topology_instances_from_groups(
    snapshot: &mut RenderWorldSnapshot,
    groups: &HashMap<String, Vec<String>>,
) {
    let mut to_remove: HashSet<usize> = HashSet::new();

    for (target_path, instance_roots) in groups {
        if instance_roots.len() < 2 {
            continue;
        }

        // For each instance root, collect snapshot mesh entries by relative sub-path.
        let mut relative_groups: HashMap<String, Vec<(String, usize)>> = HashMap::new();
        for root in instance_roots {
            let prefix = format!("{root}/");
            for (idx, inst) in snapshot.meshes.iter().enumerate() {
                if to_remove.contains(&idx) {
                    continue;
                }
                if inst.purpose != RenderPurpose::Default {
                    continue;
                }
                if inst.mesh.skin.is_some() {
                    continue;
                }
                if inst.prim_path.starts_with(&prefix) {
                    let relative = inst.prim_path[root.len()..].to_string();
                    relative_groups
                        .entry(relative)
                        .or_default()
                        .push((root.clone(), idx));
                }
            }
        }

        // Promote each relative group that is covered by every instance root and passes
        // the geometry + material fingerprint validation ("only transforms differ").
        for (relative, entries) in &relative_groups {
            if entries.len() != instance_roots.len() {
                continue;
            }

            let geo_fp = geometry_fingerprint(&snapshot.meshes[entries[0].1].mesh);
            let mat_fp = snapshot.meshes[entries[0].1].material.fingerprint;
            let cast_fp = snapshot.meshes[entries[0].1].casts_shadow;
            // `casts_shadow` is part of the batch identity: a promoted batch has a
            // single shadow-caster flag (copied from the first mesh below), so the
            // whole group must agree — otherwise non-casters would start casting,
            // or casters would vanish from the shadow pass.
            let all_match = entries.iter().skip(1).all(|(_, i)| {
                geometry_fingerprint(&snapshot.meshes[*i].mesh) == geo_fp
                    && snapshot.meshes[*i].material.fingerprint == mat_fp
                    && snapshot.meshes[*i].casts_shadow == cast_fp
            });
            if !all_match {
                continue;
            }

            let proto_key = format!("toporef_{target_path}{relative}");
            let mesh = snapshot.meshes[entries[0].1].mesh.clone();
            let material = snapshot.meshes[entries[0].1].material.clone();
            let purpose = snapshot.meshes[entries[0].1].purpose;
            let casts_shadow = snapshot.meshes[entries[0].1].casts_shadow;
            let instance_transforms: Vec<Mat4> = entries
                .iter()
                .map(|(_, i)| snapshot.meshes[*i].world_matrix)
                .collect();
            let instance_prim_paths: Vec<String> =
                entries.iter().map(|(root, _)| root.clone()).collect();

            snapshot.instanced_batches.push(RenderInstancedBatch {
                prototype_key: proto_key,
                mesh,
                instance_transforms,
                instance_prim_paths,
                material,
                purpose,
                casts_shadow,
            });
            to_remove.extend(entries.iter().map(|(_, i)| *i));
        }
    }

    if !to_remove.is_empty() {
        snapshot.stats.topology_instance_promoted += to_remove.len();
        snapshot.stats.instanced_batch_count = snapshot.instanced_batches.len();
        let mut remove_sorted: Vec<usize> = to_remove.into_iter().collect();
        remove_sorted.sort_unstable_by(|a, b| b.cmp(a));
        for i in remove_sorted {
            snapshot.meshes.swap_remove(i);
        }
        snapshot.stats.instanced_draw_count = snapshot
            .instanced_batches
            .iter()
            .map(|b| b.instance_transforms.len())
            .sum();
    }
}
