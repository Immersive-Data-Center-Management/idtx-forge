//! Triangulated GPU mesh descriptor.
//!
//! USD meshes are polygon soups (quads, ngons). This module provides
//! [`GpuMeshDesc`] — a triangulated, flat-vertex representation suitable for
//! a GPU triangle-list draw call — and helpers to compute flat normals from
//! position data when the source mesh has none.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use glam::Vec3;

static MESH_SERIAL: AtomicU64 = AtomicU64::new(1);

/// Memoized smooth-normal synthesis, keyed by prim path and validated by a
/// content fingerprint.
///
/// `GpuMeshDesc::from_mesh_comp` runs for every mesh on every dirty/full
/// `RenderWorldSnapshot::extract`, and the synthesis is an O(corners)
/// HashMap/union-find pass. Caching it keeps an unchanged mesh from
/// re-synthesizing on repeated extracts (which would otherwise run per-frame
/// during geometry animation).
///
/// The stored value carries a `fingerprint` hashed from every synthesis input
/// (see [`synthesis_fingerprint`]), NOT the mesh's `geometry_version`:
/// `geometry_version` restarts at 1 per hydrated world, so two concurrently-open
/// stages routinely share a `(prim_path, version)` key for different geometry —
/// keying on it would hand one session another session's normals (possibly a
/// different-length vector). The fingerprint is stage-independent and bounds the
/// map to one entry per prim path (replaced whenever that path's content
/// changes), while still sharing results for a stable mesh within a session.
type SynthNormalCache = HashMap<String, (u64, Vec<[f32; 3]>)>;

static SYNTH_NORMAL_CACHE: LazyLock<Mutex<SynthNormalCache>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A 64-bit fingerprint of everything `compute_smooth_normals_with_creases`
/// consumes, so an unchanged mesh hashes identically (cache hit) and any change
/// that would alter the synthesized normals produces a different hash — including
/// across stages, where `geometry_version` is not globally unique.
fn synthesis_fingerprint(mesh: &MeshComp) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write_u8(u8::from(mesh.left_handed));
    hasher.write_usize(mesh.points.len());
    for p in &mesh.points {
        for c in p {
            hasher.write_u32(c.to_bits());
        }
    }
    for &c in &mesh.face_vertex_counts {
        hasher.write_i32(c);
    }
    for &i in &mesh.face_vertex_indices {
        hasher.write_i32(i);
    }
    for &i in &mesh.crease_indices {
        hasher.write_i32(i);
    }
    for &l in &mesh.crease_lengths {
        hasher.write_i32(l);
    }
    for &s in &mesh.crease_sharpnesses {
        hasher.write_u32(s.to_bits());
    }
    for &i in &mesh.corner_indices {
        hasher.write_i32(i);
    }
    for &s in &mesh.corner_sharpnesses {
        hasher.write_u32(s.to_bits());
    }
    hasher.finish()
}

/// Synthesizes crease-aware smooth normals for `mesh`, reusing the cached result
/// when the prim's synthesis-input fingerprint is unchanged. See
/// [`SYNTH_NORMAL_CACHE`].
fn synthesized_normals_cached(prim_path: &str, mesh: &MeshComp) -> Vec<[f32; 3]> {
    let fingerprint = synthesis_fingerprint(mesh);
    let cached = {
        let cache = SYNTH_NORMAL_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache
            .get(prim_path)
            .filter(|(cached_fingerprint, _)| *cached_fingerprint == fingerprint)
            .map(|(_, normals)| normals.clone())
    };
    if let Some(normals) = cached {
        return normals;
    }

    let hard_edges = hard_edges_from_creases(
        &mesh.crease_indices,
        &mesh.crease_lengths,
        &mesh.crease_sharpnesses,
        DEFAULT_CREASE_SHARPNESS_CUTOFF,
    );
    let corner_points = corner_point_set(
        &mesh.corner_indices,
        &mesh.corner_sharpnesses,
        DEFAULT_CREASE_SHARPNESS_CUTOFF,
    );
    let normals = compute_smooth_normals_with_creases(
        &mesh.points,
        &mesh.face_vertex_counts,
        &mesh.face_vertex_indices,
        DEFAULT_CREASE_ANGLE,
        mesh.left_handed,
        &hard_edges,
        &corner_points,
    );

    let mut cache = SYNTH_NORMAL_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.insert(prim_path.to_owned(), (fingerprint, normals.clone()));
    normals
}

/// Cumulative cost of smooth-normal synthesis ([`compute_smooth_normals`]), the
/// one hydration-time path this module adds. Gated behind `metrics` so release
/// builds pay zero overhead. Read + reset with [`take_normal_metrics`] to compare
/// against the render-performance baseline and confirm the per-frame path is
/// untouched (all cost lands here, once per mesh geometry-version).
#[cfg(feature = "metrics")]
mod normal_metrics {
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(super) static NANOS: AtomicU64 = AtomicU64::new(0);
    pub(super) static CORNERS: AtomicU64 = AtomicU64::new(0);
    pub(super) static CALLS: AtomicU64 = AtomicU64::new(0);

    /// Accumulated smooth-normal synthesis counters.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct NormalMetrics {
        /// Wall time spent synthesizing smooth normals, in nanoseconds.
        pub nanos: u64,
        /// Total face-corners processed.
        pub corners: u64,
        /// Number of `compute_smooth_normals*` invocations.
        pub calls: u64,
    }

    /// Reads and resets the accumulated smooth-normal synthesis metrics.
    #[must_use]
    pub fn take_normal_metrics() -> NormalMetrics {
        NormalMetrics {
            nanos: NANOS.swap(0, Ordering::Relaxed),
            corners: CORNERS.swap(0, Ordering::Relaxed),
            calls: CALLS.swap(0, Ordering::Relaxed),
        }
    }
}
#[cfg(feature = "metrics")]
pub use normal_metrics::{NormalMetrics, take_normal_metrics};

/// Returns the next value from the crate-global mesh serial counter.
///
/// `GpuMeshDesc::serial` values must be unique across the render crate so the
/// per-instance mesh cache can reliably detect geometry changes. Callers that
/// mutate a mesh's vertex data after construction (e.g. CPU-skinned deformation
/// in `scene_extract`) use this to obtain a fresh serial from the same counter.
///
/// Public so downstream crates that synthesize a merged `GpuMeshDesc` at runtime
/// (e.g. `idtx-studio-session`'s XR controller mesh sourcing) can assign a fresh,
/// globally-unique serial to their result.
pub fn next_mesh_serial() -> u64 {
    MESH_SERIAL.fetch_add(1, Ordering::Relaxed)
}

/// Derives a **content-stable** serial from a primitive's tessellation
/// parameters, so re-tessellating the same shape yields the same serial across
/// frames.
///
/// Primitive gprims (cube/sphere/cylinder/cone/capsule) are re-tessellated on
/// every `RenderWorldSnapshot::extract`. Drawing a fresh value from the global
/// `MESH_SERIAL` counter on each call made a *static* shape look new to the
/// renderer's per-key mesh cache every frame, forcing an evict + GPU re-upload
/// (the B3 "gizmo cube rebuilt every frame" churn). Deriving the serial from the
/// shape parameters instead keeps it constant for an unchanged shape — so the
/// cache holds — while a parameter change (e.g. an animated radius) still
/// changes the hash and correctly triggers a rebuild.
///
/// The serial only needs to be stable **per cache key** (`prim_path`), so a hash
/// collision between two differently-shaped prims is harmless (each has its own
/// cache entry). `tag` disambiguates the primitive kind to keep collisions rare.
fn content_serial(tag: u64, params: &[f32]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tag.hash(&mut hasher);
    for &p in params {
        p.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

use idtx_scene::MeshComp;
use idtx_scene::hydrator::triangulate_mesh;
use idtx_usd::GprimAxis;

use crate::convert::{AsF32, u32_sat};
use crate::vertex::SkinVertex;

/// Per-vertex skinning data for GPU classic-linear blend skinning.
///
/// `influences` is in flat-vertex (face-corner expanded) order — one entry per
/// vertex in the parent [`GpuMeshDesc`], obtained by expanding per-point
/// `SkinBindingRecord::influences` through `MeshComp::face_vertex_indices`.
///
/// `joint_matrices` are pre-multiplied effective matrices:
/// `effective_j = skeleton_world × skinning_transform_j × geom_bind`.
/// They are in column-major glam convention (already converted from USD row-major).
/// At most 256 entries to stay within the 16 KB WebGPU uniform buffer limit.
#[derive(Debug, Clone)]
pub struct SkinData {
    pub influences: Vec<SkinVertex>,
    pub joint_matrices: Vec<[f32; 16]>,
}

/// Per-vertex data for a single triangle-list mesh, ready for GPU buffer upload.
///
/// All arrays have equal length `vertex_count`. The index buffer references
/// into these arrays.
#[derive(Debug, Clone)]
pub struct GpuMeshDesc {
    /// Unique identifier — the USD prim path string of the source mesh.
    pub prim_path: String,
    /// XYZ positions in local prim space.
    pub positions: Vec<[f32; 3]>,
    /// Per-vertex normals (smooth or flat, computed if absent in source).
    pub normals: Vec<[f32; 3]>,
    /// Per-vertex tangents (xyz + bitangent sign w). Empty when not authored.
    pub tangents: Vec<[f32; 4]>,
    /// Primary UV set (ST), or `[0,0]` when the source mesh has no texcoords.
    pub texcoords: Vec<[f32; 2]>,
    /// Per-vertex color tint from displayColor primvar (face-corner expanded).
    /// White `[1,1,1]` when the source mesh has no displayColor.
    pub vertex_colors: Vec<[f32; 3]>,
    /// Triangle-list index buffer (every 3 consecutive indices form one triangle).
    pub indices: Vec<u32>,
    /// Winding order is right-handed (CCW front-facing) after handedness correction.
    pub double_sided: bool,
    /// USD material path to resolve from the scene's material map.
    pub material_path: Option<String>,
    /// GPU skeletal skinning data. `Some` only for `GpuClassicLinear` meshes;
    /// `None` for static, `CpuOpenUsd` (positions pre-deformed), and `RestPose` meshes.
    pub skin: Option<SkinData>,
    /// Monotonically increasing serial assigned at construction time.
    /// The renderer compares this against `CachedMeshGpu::last_mesh_serial` to detect
    /// when geometry changed for the same prim path (e.g. structural variant switch).
    pub serial: u64,
    /// Local-space axis-aligned bounds `(min, max)` of [`positions`](Self::positions),
    /// computed once at construction. `None` for an empty mesh. Lets
    /// [`compute_scene_aabb`](crate::camera_fit::compute_scene_aabb) transform 8
    /// corners per mesh (O(meshes)) instead of walking every vertex every frame
    /// (O(vertices)) — the fix for the per-frame shadow-AABB refit cost.
    pub local_aabb: Option<([f32; 3], [f32; 3])>,
}

/// Local-space AABB `(min, max)` of a position list, or `None` when empty. The
/// single source of truth for [`GpuMeshDesc::local_aabb`]; also used by the
/// shadow-refit bounds fast-path.
#[must_use]
pub fn aabb_of_positions(positions: &[[f32; 3]]) -> Option<([f32; 3], [f32; 3])> {
    let mut it = positions.iter();
    let first = it.next()?;
    let mut min = *first;
    let mut max = *first;
    for p in it {
        for a in 0..3 {
            min[a] = min[a].min(p[a]);
            max[a] = max[a].max(p[a]);
        }
    }
    Some((min, max))
}

impl GpuMeshDesc {
    /// Triangulates a [`MeshComp`] and computes flat normals.
    ///
    /// `prim_path` is the USD path string from [`idtx_scene::PrimNode::path`].
    #[must_use]
    pub fn from_mesh_comp(prim_path: impl Into<String>, mesh: &MeshComp) -> Self {
        let prim_path = prim_path.into();
        let (positions, mut texcoords, mut indices) = triangulate_mesh(
            &mesh.points,
            &mesh.texcoords,
            &mesh.face_vertex_counts,
            &mesh.face_vertex_indices,
        );

        // USD uses a bottom-left UV origin (V = 0 at bottom).  wgpu textures
        // use a top-left origin (V = 0 at top).  Flip V so texture sampling is
        // correct without any shader-side compensation.
        //
        // When a normal-map tangent frame is later derived from screen-space
        // derivatives (dpdx/dpdy) in the fragment shader, it will already be in
        // the flipped UV space and will therefore be correct.
        for uv in &mut texcoords {
            uv[1] = 1.0 - uv[1];
        }

        // Correct winding for left-handed USD meshes (flip every triangle).
        if mesh.left_handed {
            for tri in indices.chunks_exact_mut(3) {
                tri.swap(1, 2);
            }
        }

        // Use USD normals when authored (already in face-corner order). When the
        // mesh has none, synthesize smooth adjacency-averaged normals with a
        // crease-angle hard-edge split — the default that makes untouched assets
        // read as smooth rather than faceted, at zero per-frame cost (same vertex
        // layout, same shader; only these hydration-time values differ).
        //
        // Authored `UsdGeomMesh` creases/corners (Phase 2) are honoured as forced
        // hard edges / sharp vertices so a subdivision asset's sharp features stay
        // faceted even though we render the un-subdivided control cage.
        //
        // The synthesis is a HashMap/union-find pass over the whole mesh;
        // `from_mesh_comp` is re-run for every mesh on every dirty/full
        // `RenderWorldSnapshot::extract`, so it is memoized by
        // `(prim_path, geometry_version)` — the synthesis inputs all change the
        // geometry version (see `mesh_geometry_unchanged`) — to keep repeated
        // extracts of an unchanged mesh from re-running it (per-frame during
        // geometry animation otherwise).
        let normals = if mesh.normals.len() == positions.len() {
            mesh.normals.clone()
        } else {
            synthesized_normals_cached(&prim_path, mesh)
        };

        // Use USD tangents when available; leave empty otherwise (normal mapping deferred).
        //
        // The V flip above (`v → 1 - v`) reverses the handedness of the UV
        // parametrisation, so the authored bitangent sign `tangent.w` — which
        // the shader consumes as `bitangent = cross(N, T) * w` — must be negated
        // to keep the per-vertex TBN frame consistent with the flipped-V UVs the
        // fragment shader samples the normal map with (and with the screen-space
        // derivative TBN used when no tangents are authored). Without this,
        // normal mapping builds the wrong-handed bitangent and can drive the
        // perturbed normal away from every light, rendering the surface black.
        // UDIM meshes undo the V flip in `unflip_texcoords_v`, which re-negates w
        // so the two operations remain inverses.
        let tangents = if mesh.tangents.len() == positions.len() {
            mesh.tangents
                .iter()
                .map(|t| [t[0], t[1], t[2], -t[3]])
                .collect()
        } else {
            Vec::new()
        };

        // `primvars:displayColor` is the UsdGeom *fallback* display opinion: USD/Hydra
        // consume it only when no surface material is bound (or in an explicit
        // displayColor draw mode). When a material with a surface terminal is bound, the
        // material defines the colour and displayColor must be ignored — otherwise an
        // exporter's placeholder `displayColor = (0,0,0)` (common in RealityKit/Apple
        // `.usdz` exports) would zero the albedo in the shader's `base_color * vertex_color`
        // tint and render the whole textured mesh black. So carry displayColor into the
        // vertex-colour tint only for unmaterialed geometry; bound meshes get the identity
        // white tint (a no-op multiply), matching usdview.
        //
        // A binding counts whether it is on the mesh itself (`material_path`) OR on a
        // `UsdGeomSubset` (`material_subsets`): RealityKit exports frequently leave the
        // mesh unbound and bind a single `partition`-family subset covering every face,
        // so a subset binding governs the colour just as a whole-mesh binding would.
        let has_bound_material = mesh.material_path.as_deref().is_some_and(|p| !p.is_empty())
            || mesh
                .material_subsets
                .iter()
                .any(|s| s.material_path.as_deref().is_some_and(|p| !p.is_empty()));
        let vertex_colors = if !has_bound_material && mesh.display_colors.len() == positions.len() {
            mesh.display_colors.clone()
        } else {
            vec![[1.0_f32, 1.0, 1.0]; positions.len()]
        };

        Self {
            prim_path,
            local_aabb: aabb_of_positions(&positions),
            positions,
            normals,
            tangents,
            texcoords,
            vertex_colors,
            indices,
            double_sided: mesh.double_sided,
            material_path: mesh.material_path.clone(),
            skin: None,
            serial: mesh.geometry_version,
        }
    }

    /// Number of triangles in this mesh.
    #[must_use]
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Re-applies the `v → 1 - v` flip that [`from_mesh_comp`] bakes in for
    /// wgpu's top-left texture origin, restoring the **raw USD-space** V.
    ///
    /// UDIM tile addressing needs raw UVs (a tile is `floor(v)`, and v may
    /// exceed 1), so UDIM meshes are emitted un-flipped and the per-tile
    /// `local_v = 1 - fract(v)` flip is done in the shader instead. Applying
    /// this to a mesh built by `from_mesh_comp` (already flipped) yields the
    /// original USD V; calling it twice is the identity.
    ///
    /// The bitangent sign `tangent.w`, which [`from_mesh_comp`] negates to match
    /// the V flip, is re-negated here so unflipping restores the raw-UV tangent
    /// frame as well — keeping this a true inverse of the flip.
    ///
    /// [`from_mesh_comp`]: GpuMeshDesc::from_mesh_comp
    pub fn unflip_texcoords_v(&mut self) {
        for uv in &mut self.texcoords {
            uv[1] = 1.0 - uv[1];
        }
        for t in &mut self.tangents {
            t[3] = -t[3];
        }
    }

    /// Number of vertices in this mesh.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    /// Extracts a subset of triangles from a fully-triangulated `GpuMeshDesc`
    /// based on the original (pre-triangulation) face indices.
    ///
    /// Used for `UsdGeomSubset` material binding: each subset references a set of
    /// source faces, and this function produces a new `GpuMeshDesc` containing only
    /// those triangles, with indices remapped into the new contiguous vertex buffer.
    ///
    /// `face_vertex_counts` is the original USD polygon-soup face-count array
    /// (same as `MeshComp::face_vertex_counts`). `face_indices` are the 0-based face
    /// numbers belonging to this subset.
    ///
    /// Degenerate faces (fewer than 3 vertices) are handled: they contribute no
    /// vertices or triangles to the output even if referenced.
    #[must_use]
    pub fn subset_by_faces(
        full: &GpuMeshDesc,
        face_vertex_counts: &[i32],
        face_indices: &[u32],
    ) -> GpuMeshDesc {
        // Build per-face lookup tables: where does each face start in the flat
        // vertex buffer, and where do its triangles start in the index buffer?
        // Both tables skip degenerate faces (n < 3), matching triangulate_mesh behaviour.
        let mut flat_start_per_face: Vec<usize> = Vec::with_capacity(face_vertex_counts.len());
        let mut tri_start_per_face: Vec<usize> = Vec::with_capacity(face_vertex_counts.len());
        let mut flat_cursor = 0_usize;
        let mut tri_cursor = 0_usize;
        for &c in face_vertex_counts {
            flat_start_per_face.push(flat_cursor);
            tri_start_per_face.push(tri_cursor);
            let n = usize::try_from(c).unwrap_or(0);
            if n >= 3 {
                flat_cursor += n;
                tri_cursor += n - 2;
            }
        }

        let mut out_positions: Vec<[f32; 3]> = Vec::new();
        let mut out_normals: Vec<[f32; 3]> = Vec::new();
        let mut out_texcoords: Vec<[f32; 2]> = Vec::new();
        let mut out_vertex_colors: Vec<[f32; 3]> = Vec::new();
        let mut out_tangents: Vec<[f32; 4]> = Vec::new();
        let mut out_indices: Vec<u32> = Vec::new();
        let mut out_influences: Vec<SkinVertex> = Vec::new();

        for &fi in face_indices {
            let fi = fi as usize;
            let Some(&c) = face_vertex_counts.get(fi) else {
                continue;
            };
            let n = usize::try_from(c).unwrap_or(0);
            if n < 3 {
                continue;
            }

            let flat_start = flat_start_per_face[fi];
            let tri_start = tri_start_per_face[fi];
            let new_base = u32_sat(out_positions.len());

            // Copy the n flat vertices for this face.
            for k in flat_start..flat_start + n {
                out_positions.push(full.positions.get(k).copied().unwrap_or([0.0; 3]));
                out_normals.push(full.normals.get(k).copied().unwrap_or([0.0, 1.0, 0.0]));
                out_texcoords.push(full.texcoords.get(k).copied().unwrap_or([0.0; 2]));
                out_vertex_colors.push(full.vertex_colors.get(k).copied().unwrap_or([1.0; 3]));
                if !full.tangents.is_empty() {
                    out_tangents.push(
                        full.tangents
                            .get(k)
                            .copied()
                            .unwrap_or([1.0, 0.0, 0.0, 1.0]),
                    );
                }
                if full.skin.is_some() {
                    let fallback = SkinVertex {
                        joints: [0; 8],
                        weights: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                    };
                    out_influences.push(
                        full.skin
                            .as_ref()
                            .unwrap()
                            .influences
                            .get(k)
                            .copied()
                            .unwrap_or(fallback),
                    );
                }
            }

            // Copy and remap the (n-2) triangles (fan triangulation, possibly winding-flipped).
            // Original indices reference into flat_start..flat_start+n; remap to new_base..
            let idx_start = tri_start * 3;
            let idx_end = idx_start + (n - 2) * 3;
            for k in idx_start..idx_end {
                if let Some(&old_idx) = full.indices.get(k) {
                    let offset = old_idx.saturating_sub(u32_sat(flat_start));
                    out_indices.push(new_base + offset);
                }
            }
        }

        GpuMeshDesc {
            prim_path: full.prim_path.clone(),
            local_aabb: aabb_of_positions(&out_positions),
            positions: out_positions,
            normals: out_normals,
            tangents: out_tangents,
            texcoords: out_texcoords,
            vertex_colors: out_vertex_colors,
            indices: out_indices,
            double_sided: full.double_sided,
            material_path: full.material_path.clone(),
            skin: full.skin.as_ref().map(|s| SkinData {
                influences: out_influences,
                joint_matrices: s.joint_matrices.clone(),
            }),
            serial: full.serial,
        }
    }
}

// ── Normal computation ────────────────────────────────────────────────────────

/// Computes flat (face) normals for a triangle-list mesh.
///
/// Each triangle's normal is assigned to all three of its vertices. The result
/// is suitable for flat shading. Smooth normals require welding (Phase 3).
#[must_use]
pub fn compute_flat_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut normals = vec![[0.0_f32; 3]; positions.len()];
    for tri in indices.chunks_exact(3) {
        let Some([i0, i1, i2]) = tri
            .try_into()
            .ok()
            .map(|[a, b, c]: [u32; 3]| [a as usize, b as usize, c as usize])
        else {
            continue;
        };
        let (Some(&p0), Some(&p1), Some(&p2)) =
            (positions.get(i0), positions.get(i1), positions.get(i2))
        else {
            continue;
        };
        let v0 = Vec3::from(p0);
        let v1 = Vec3::from(p1);
        let v2 = Vec3::from(p2);
        let n = (v1 - v0).cross(v2 - v0).normalize_or_zero();
        let nf = [n.x, n.y, n.z];
        normals[i0] = nf;
        normals[i1] = nf;
        normals[i2] = nf;
    }
    normals
}

// ── Smooth normal computation ───────────────────────────────────────────────

/// Union-find root with path halving, used to merge mesh faces into smoothing
/// groups across sub-crease-angle edges.
fn uf_find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

/// Crease angle (radians) above which a face-corner keeps its own flat face
/// normal instead of the point-averaged smooth normal.
///
/// 60° smooths curved, finely-tessellated surfaces (sphere facets, cylinder
/// barrels) while keeping genuine hard edges sharp: a cube's 90° dihedral, a
/// cylinder's side↔cap seam, and a cone's base ring all exceed this threshold
/// and stay faceted. Matches the "smoothing angle" knob common to DCC tools.
pub const DEFAULT_CREASE_ANGLE: f32 = std::f32::consts::FRAC_PI_3;

/// Authored crease/corner sharpness at or above which a feature is treated as a
/// hard edge/vertex by the smooth-normal synthesis.
///
/// USD sharpness runs 0 (smooth) to 10 (`SdfMesh` "infinitely sharp"). Because we
/// render the un-subdivided control cage rather than a true limit surface, any
/// deliberately-authored crease (sharpness ≥ 1) is snapped to a hard edge.
pub const DEFAULT_CREASE_SHARPNESS_CUTOFF: f32 = 1.0;

/// Computes **smooth** (adjacency-averaged) per-corner normals for a polygon mesh
/// that authored no `normals` primvar, with a crease-angle hard-edge split.
///
/// This is the default fallback for [`GpuMeshDesc::from_mesh_comp`] — it replaces
/// the old per-face [`compute_flat_normals`] fallback that made every asset look
/// faceted. Normals are welded across faces that share a point and a *smooth*
/// edge (dihedral angle ≤ `crease_angle`); faces separated by a sharper edge keep
/// independent normals so the edge reads as hard.
///
/// Inputs are USD **point-space** arrays (pre-triangulation), identical to what
/// [`crate::triangulate_mesh`] consumes; the output is in the **same face-corner
/// order** that `triangulate_mesh` emits positions in (faces with fewer than 3
/// corners are skipped, matching it exactly), so the returned `Vec` lines up 1:1
/// with the triangulated position buffer.
///
/// `left_handed` mirrors `MeshComp::left_handed`: the renderer flips left-handed
/// winding so every triangle is right-handed/CCW-front, so a left-handed mesh's
/// synthesized normals are negated to point along that corrected winding.
#[must_use]
pub fn compute_smooth_normals(
    points: &[[f32; 3]],
    face_vertex_counts: &[i32],
    face_vertex_indices: &[i32],
    crease_angle: f32,
    left_handed: bool,
) -> Vec<[f32; 3]> {
    compute_smooth_normals_with_creases(
        points,
        face_vertex_counts,
        face_vertex_indices,
        crease_angle,
        left_handed,
        &HashSet::new(),
        &HashSet::new(),
    )
}

/// [`compute_smooth_normals`] with explicit sharp features from a subdivision
/// surface (Phase 2): `hard_edges` are additional undirected point-index edges
/// (each stored as `(min, max)`) that are always treated as creases regardless of
/// dihedral angle, and `corner_points` are point indices whose incident faces are
/// each kept flat (a sharp vertex).
///
/// `hard_edges` are typically built from `UsdGeomMesh` `creaseIndices` /
/// `creaseLengths` / `creaseSharpness`, and `corner_points` from `cornerIndices` /
/// `cornerSharpness` — see [`hard_edges_from_creases`] and [`corner_point_set`].
#[must_use]
#[allow(
    clippy::implicit_hasher,
    clippy::many_single_char_names,
    reason = "math-heavy mesh code; matches the single-letter idiom used by the tessellate_* fns in this module, and callers always pass the default-hasher sets built by hard_edges_from_creases/corner_point_set"
)]
pub fn compute_smooth_normals_with_creases(
    points: &[[f32; 3]],
    face_vertex_counts: &[i32],
    face_vertex_indices: &[i32],
    crease_angle: f32,
    left_handed: bool,
    hard_edges: &HashSet<(u32, u32)>,
    corner_points: &HashSet<u32>,
) -> Vec<[f32; 3]> {
    if points.is_empty() {
        return Vec::new();
    }
    let point_count = points.len();
    #[cfg(feature = "metrics")]
    let start = std::time::Instant::now();

    // ── Pass 1: emitted-face corner points + area-weighted face normals ──────
    // Iterate faces exactly as `triangulate_mesh` does: skip faces with < 3
    // corners (advancing the cursor), so our output aligns with the positions.
    let mut corner_pt: Vec<u32> = Vec::new(); // point index per emitted corner
    let mut face_start: Vec<usize> = Vec::new(); // offset into corner_pt per face
    let mut face_len: Vec<u32> = Vec::new();
    let mut face_normal: Vec<Vec3> = Vec::new(); // area-weighted, sign-corrected

    let mut cursor: usize = 0;
    for &count in face_vertex_counts {
        let n = usize::try_from(count).unwrap_or(0);
        if n < 3 {
            cursor += n;
            continue;
        }
        let start = corner_pt.len();
        for k in 0..n {
            let raw = face_vertex_indices.get(cursor + k).copied().unwrap_or(0);
            let vi = usize::try_from(raw).unwrap_or(0).min(point_count - 1);
            corner_pt.push(u32_sat(vi));
        }
        // Newell's method: robust for non-planar polygons; the un-normalized
        // result's magnitude is proportional to face area (area weighting).
        let mut nrm = Vec3::ZERO;
        for k in 0..n {
            let a = Vec3::from(points[corner_pt[start + k] as usize]);
            let b = Vec3::from(points[corner_pt[start + (k + 1) % n] as usize]);
            nrm.x += (a.y - b.y) * (a.z + b.z);
            nrm.y += (a.z - b.z) * (a.x + b.x);
            nrm.z += (a.x - b.x) * (a.y + b.y);
        }
        if left_handed {
            nrm = -nrm;
        }
        face_start.push(start);
        face_len.push(u32_sat(n));
        face_normal.push(nrm);
        cursor += n;
    }
    let total_corners = corner_pt.len();
    let face_count = face_normal.len();

    // ── Build undirected edge → incident faces ───────────────────────────────
    let mut edge_faces: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for f in 0..face_count {
        let s = face_start[f];
        let n = face_len[f] as usize;
        for k in 0..n {
            let a = corner_pt[s + k];
            let b = corner_pt[s + (k + 1) % n];
            let key = if a <= b { (a, b) } else { (b, a) };
            edge_faces.entry(key).or_default().push(f);
        }
    }

    // ── Per-point smoothing groups ───────────────────────────────────────────
    // A point is shared by several face-corners; two corners at the SAME point
    // merge into one smoothing group only when their faces meet across a smooth
    // edge INCIDENT TO THAT POINT. Grouping per point (rather than unioning whole
    // faces globally) means a hard/creased edge cannot be bypassed by a chain of
    // smooth edges elsewhere in the topology — the classic failure where a single
    // hard seam on an otherwise-smooth closed surface (a cylinder, a tube) gets
    // averaged away because its two faces are still connected the long way round.
    //
    // Union-find nodes are the incident corners, keyed by (point, face). Every
    // union connects two corners that share a point, so each connected component
    // stays within one point and encodes exactly one (point, smoothing-group).
    let mut node_of: HashMap<(u32, usize), usize> = HashMap::new();
    for f in 0..face_count {
        let s = face_start[f];
        let n = face_len[f] as usize;
        for k in 0..n {
            let p = corner_pt[s + k];
            let next = node_of.len();
            node_of.entry((p, f)).or_insert(next);
        }
    }
    let mut parent: Vec<usize> = (0..node_of.len()).collect();
    let cos_thresh = crease_angle.cos();
    for (key, faces) in &edge_faces {
        // A manifold edge joins exactly two faces. Boundary/non-manifold edges,
        // authored creases, and sharp dihedrals all stay hard (no union).
        if faces.len() != 2 || hard_edges.contains(key) {
            continue;
        }
        let n0 = face_normal[faces[0]].normalize_or_zero();
        let n1 = face_normal[faces[1]].normalize_or_zero();
        if n0 == Vec3::ZERO || n1 == Vec3::ZERO {
            continue;
        }
        if n0.dot(n1) < cos_thresh {
            continue; // sharp dihedral — keep hard
        }
        // Smooth edge: merge its two faces at BOTH shared endpoints, except where
        // an endpoint is an authored corner (sharp vertex), which stays flat so
        // each incident face keeps its own normal there.
        let (a, b) = *key;
        for &p in &[a, b] {
            if corner_points.contains(&p) {
                continue;
            }
            if let (Some(&na), Some(&nb)) =
                (node_of.get(&(p, faces[0])), node_of.get(&(p, faces[1])))
            {
                let ra = uf_find(&mut parent, na);
                let rb = uf_find(&mut parent, nb);
                if ra != rb {
                    parent[ra] = rb;
                }
            }
        }
    }

    // ── Accumulate area-weighted face normals per smoothing group ─────────────
    let mut accum: HashMap<usize, Vec3> = HashMap::new();
    for f in 0..face_count {
        let s = face_start[f];
        let n = face_len[f] as usize;
        for k in 0..n {
            let p = corner_pt[s + k];
            let root = uf_find(&mut parent, node_of[&(p, f)]);
            *accum.entry(root).or_default() += face_normal[f];
        }
    }

    // ── Emit per corner, in triangulate_mesh order ───────────────────────────
    let mut out = vec![[0.0_f32, 1.0, 0.0]; total_corners];
    let mut idx = 0_usize;
    for f in 0..face_count {
        let s = face_start[f];
        let n = face_len[f] as usize;
        for k in 0..n {
            let p = corner_pt[s + k];
            let root = uf_find(&mut parent, node_of[&(p, f)]);
            let summed = accum.get(&root).copied().unwrap_or(face_normal[f]);
            let mut v = summed.normalize_or_zero();
            if v == Vec3::ZERO {
                v = face_normal[f].normalize_or_zero();
            }
            if v != Vec3::ZERO {
                out[idx] = [v.x, v.y, v.z];
            }
            idx += 1;
        }
    }
    #[cfg(feature = "metrics")]
    {
        use std::sync::atomic::Ordering;
        normal_metrics::NANOS.fetch_add(
            crate::convert::nanos_u64(start.elapsed().as_nanos()),
            Ordering::Relaxed,
        );
        normal_metrics::CORNERS
            .fetch_add(u64::try_from(total_corners).unwrap_or(u64::MAX), Ordering::Relaxed);
        normal_metrics::CALLS.fetch_add(1, Ordering::Relaxed);
    }
    out
}

/// Builds the set of undirected hard edges (as `(min, max)` point-index pairs)
/// from a `UsdGeomMesh` crease specification.
///
/// `crease_indices` is a flat list of point indices partitioned by
/// `crease_lengths` into chains; a chain of `m` points contributes `m - 1`
/// consecutive edges. `crease_sharpness` holds either one value per crease
/// (chain) or one value per edge; an edge is treated as hard when its sharpness
/// is at least `sharpness_cutoff` (USD's "infinitely sharp" convention is 10).
#[must_use]
pub fn hard_edges_from_creases(
    crease_indices: &[i32],
    crease_lengths: &[i32],
    crease_sharpness: &[f32],
    sharpness_cutoff: f32,
) -> HashSet<(u32, u32)> {
    let mut edges = HashSet::new();
    // Does crease_sharpness carry one value per edge, or one per crease?
    let total_edges: usize = crease_lengths
        .iter()
        .map(|&l| usize::try_from(l).unwrap_or(0).saturating_sub(1))
        .sum();
    let per_edge = crease_sharpness.len() == total_edges;

    let mut cursor = 0_usize; // into crease_indices
    let mut edge_cursor = 0_usize; // into crease_sharpness (per-edge mode)
    for (crease, &len) in crease_lengths.iter().enumerate() {
        let m = usize::try_from(len).unwrap_or(0);
        for seg in 0..m.saturating_sub(1) {
            let sharp = if per_edge {
                crease_sharpness.get(edge_cursor).copied().unwrap_or(0.0)
            } else {
                crease_sharpness.get(crease).copied().unwrap_or(0.0)
            };
            edge_cursor += 1;
            let a = crease_indices.get(cursor + seg).copied().unwrap_or(0);
            let b = crease_indices.get(cursor + seg + 1).copied().unwrap_or(0);
            if sharp >= sharpness_cutoff {
                let (a, b) = (
                    u32_sat(usize::try_from(a).unwrap_or(0)),
                    u32_sat(usize::try_from(b).unwrap_or(0)),
                );
                edges.insert(if a <= b { (a, b) } else { (b, a) });
            }
        }
        cursor += m;
    }
    edges
}

/// Builds the set of sharp corner point indices from a `UsdGeomMesh` corner
/// specification: a point in `corner_indices` whose paired `corner_sharpness` is
/// at least `sharpness_cutoff` becomes a sharp vertex (all its incident faces are
/// kept flat).
#[must_use]
pub fn corner_point_set(
    corner_indices: &[i32],
    corner_sharpness: &[f32],
    sharpness_cutoff: f32,
) -> HashSet<u32> {
    let mut set = HashSet::new();
    for (i, &idx) in corner_indices.iter().enumerate() {
        let sharp = corner_sharpness.get(i).copied().unwrap_or(0.0);
        if sharp >= sharpness_cutoff {
            set.insert(u32_sat(usize::try_from(idx).unwrap_or(0)));
        }
    }
    set
}

/// Generates tessellated positions and indices for a unit cube with half-extent `half`.
/// Used to represent `UsdGeomCube` prims in the render world.
#[must_use]
pub fn tessellate_cube(half: f32) -> GpuMeshDesc {
    let h = half;
    // 8 corners, 6 faces, 4 verts per face, 2 triangles per face.
    #[rustfmt::skip]
    let corners: [[f32; 3]; 8] = [
        [-h, -h, -h], [ h, -h, -h], [ h,  h, -h], [-h,  h, -h], // front
        [-h, -h,  h], [ h, -h,  h], [ h,  h,  h], [-h,  h,  h], // back
    ];
    // Each face: 4 vertex indices (CCW when viewed from outside).
    #[rustfmt::skip]
    let faces: [[usize; 4]; 6] = [
        [0, 1, 2, 3], // -Z (front)
        [5, 4, 7, 6], // +Z (back)
        [4, 0, 3, 7], // -X (left)
        [1, 5, 6, 2], // +X (right)
        [3, 2, 6, 7], // +Y (top)
        [4, 5, 1, 0], // -Y (bottom)
    ];
    let face_uvs: [[f32; 2]; 4] = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];

    let mut positions = Vec::with_capacity(24);
    let mut texcoords = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);

    for face in &faces {
        let base = u32_sat(positions.len());
        for (k, &vi) in face.iter().enumerate() {
            positions.push(corners[vi]);
            texcoords.push(face_uvs[k]);
        }
        // CCW from outside: reverse both triangles (base+1,+2 → base+2,+1 etc.)
        indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
    }

    let normals = compute_flat_normals(&positions, &indices);
    GpuMeshDesc {
        prim_path: String::new(),
        local_aabb: aabb_of_positions(&positions),
        positions,
        normals,
        tangents: Vec::new(),
        texcoords,
        vertex_colors: Vec::new(),
        indices,
        double_sided: false,
        material_path: None,
        skin: None,
        serial: content_serial(1, &[half]),
    }
}

/// Generates a double-sided flat rectangle for a `UsdGeomPlane`.
///
/// The plane is centred at the origin. `axis` is the plane normal; `width` and
/// `length` lie on the two axes perpendicular to it, per the `UsdGeomPlane`
/// schema:
///
/// | axis | width axis | length axis |
/// |------|-----------|-------------|
/// | X    | Z         | Y           |
/// | Y    | X         | Z           |
/// | Z    | X         | Y           |
///
/// Emits 4 vertices / 2 triangles. Two-sidedness is a render-state concern
/// (`GpuMeshDesc::double_sided`), so a single pair of triangles is produced and
/// the flat normal points along `+axis`. The length basis is derived as
/// `axis × width` so the winding yields that normal directly for every axis
/// (a pure rotation of one canonical quad cannot preserve the table above).
#[must_use]
pub fn tessellate_plane(width: f32, length: f32, axis: GprimAxis) -> GpuMeshDesc {
    let hw = width * 0.5;
    let hl = length * 0.5;
    let (normal, width_axis, axis_tag) = match axis {
        GprimAxis::X => (Vec3::X, Vec3::Z, 0.0_f32),
        GprimAxis::Y => (Vec3::Y, Vec3::X, 1.0_f32),
        GprimAxis::Z => (Vec3::Z, Vec3::X, 2.0_f32),
    };
    // length_axis = normal × width_axis guarantees width_axis × length_axis == normal,
    // so the (0,1,2)/(0,2,3) winding below produces a +normal flat normal.
    let length_axis = normal.cross(width_axis);
    let w = width_axis * hw;
    let l = length_axis * hl;
    let corners = [-w - l, w - l, w + l, -w + l];
    let positions: Vec<[f32; 3]> = corners.iter().map(|c| [c.x, c.y, c.z]).collect();
    let texcoords = vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
    let indices = vec![0_u32, 1, 2, 0, 2, 3];
    let normals = compute_flat_normals(&positions, &indices);
    GpuMeshDesc {
        prim_path: String::new(),
        local_aabb: aabb_of_positions(&positions),
        positions,
        normals,
        tangents: Vec::new(),
        texcoords,
        vertex_colors: Vec::new(),
        indices,
        double_sided: true,
        material_path: None,
        skin: None,
        serial: content_serial(6, &[width, length, axis_tag]),
    }
}

/// Generates a tessellated UV sphere for `UsdGeomSphere` prims.
/// `stacks` and `slices` control tessellation quality (minimum 3 each).
#[must_use]
pub fn tessellate_sphere(radius: f32, stacks: u32, slices: u32) -> GpuMeshDesc {
    let stacks = stacks.max(3);
    let slices = slices.max(3);
    let mut positions = Vec::new();
    let mut texcoords = Vec::new();
    let mut indices = Vec::new();

    for i in 0..=stacks {
        let phi = std::f32::consts::PI * i.as_f32() / stacks.as_f32();
        let sin_phi = phi.sin();
        let cos_phi = phi.cos();
        for j in 0..=slices {
            let theta = 2.0 * std::f32::consts::PI * j.as_f32() / slices.as_f32();
            let x = sin_phi * theta.cos();
            let y = cos_phi;
            let z = sin_phi * theta.sin();
            positions.push([x * radius, y * radius, z * radius]);
            texcoords.push([
                j.as_f32() / slices.as_f32(),
                1.0 - i.as_f32() / stacks.as_f32(), // V-flip: USD bottom-up → wgpu top-down
            ]);
        }
    }
    for i in 0..stacks {
        for j in 0..slices {
            let a = i * (slices + 1) + j;
            let b = a + slices + 1;
            // CCW from outside: a→a+1→b and b→a+1→b+1
            indices.extend_from_slice(&[a, a + 1, b, b, a + 1, b + 1]);
        }
    }

    let normals: Vec<[f32; 3]> = positions
        .iter()
        .map(|&p| {
            let v = Vec3::from(p).normalize_or_zero();
            [v.x, v.y, v.z]
        })
        .collect();

    GpuMeshDesc {
        prim_path: String::new(),
        local_aabb: aabb_of_positions(&positions),
        positions,
        normals,
        tangents: Vec::new(),
        texcoords,
        vertex_colors: Vec::new(),
        indices,
        double_sided: false,
        material_path: None,
        skin: None,
        serial: content_serial(2, &[radius, stacks.as_f32(), slices.as_f32()]),
    }
}

/// Generates a tessellated cylinder for `UsdGeomCylinder` prims.
///
/// Centred at the origin, axis along Y, caps at ±`half_height`.
/// Side vertices carry smooth radial normals; cap vertices carry flat ±Y normals.
/// All faces are CCW from outside (outward normal verified via cross product).
/// `slices` is the polygon count around the circumference (minimum 3).
#[must_use]
pub fn tessellate_cylinder(radius: f32, half_height: f32, slices: u32) -> GpuMeshDesc {
    let n = slices.max(3) as usize;
    let two_pi = 2.0 * std::f32::consts::PI;

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut texcoords: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // ── Side ─────────────────────────────────────────────────────────────────
    // 2n vertices: interleaved (bottom_j, top_j) pairs.
    // Triangles [bottom_j, top_j, top_{j+1}] + [bottom_j, top_{j+1}, bottom_{j+1}]:
    // v1=(0,h,0), v2≈(0,h,r·dθ) → cross = (+X at θ=0) ✓ outward.
    for j in 0..n {
        let theta = two_pi * j.as_f32() / n.as_f32();
        let (s, c) = theta.sin_cos();
        positions.push([c * radius, -half_height, s * radius]);
        normals.push([c, 0.0, s]);
        texcoords.push([j.as_f32() / n.as_f32(), 0.0]);

        positions.push([c * radius, half_height, s * radius]);
        normals.push([c, 0.0, s]);
        texcoords.push([j.as_f32() / n.as_f32(), 1.0]);
    }
    for j in 0..n {
        let jn = (j + 1) % n;
        let b0 = u32_sat(j * 2);
        let t0 = u32_sat(j * 2 + 1);
        let b1 = u32_sat(jn * 2);
        let t1 = u32_sat(jn * 2 + 1);
        indices.extend_from_slice(&[b0, t0, t1, b0, t1, b1]);
    }

    // ── Top cap (+Y) ─────────────────────────────────────────────────────────
    // Fan [centre, rim[j+1], rim[j]]: normal = +Y ✓ (sin(dθ)>0 after cross product).
    let top_centre = u32_sat(positions.len());
    positions.push([0.0, half_height, 0.0]);
    normals.push([0.0, 1.0, 0.0]);
    texcoords.push([0.5, 0.5]);
    let top_rim = u32_sat(positions.len());
    for j in 0..n {
        let theta = two_pi * j.as_f32() / n.as_f32();
        let (s, c) = theta.sin_cos();
        positions.push([c * radius, half_height, s * radius]);
        normals.push([0.0, 1.0, 0.0]);
        texcoords.push([0.5 + 0.5 * c, 0.5 + 0.5 * s]);
    }
    for j in 0..n {
        let r0 = top_rim + u32_sat(j);
        let r1 = top_rim + u32_sat((j + 1) % n);
        indices.extend_from_slice(&[top_centre, r1, r0]);
    }

    // ── Bottom cap (−Y) ──────────────────────────────────────────────────────
    // Fan [centre, rim[j], rim[j+1]]: normal = −Y ✓ (−sin(dθ)<0 after cross product).
    let bot_centre = u32_sat(positions.len());
    positions.push([0.0, -half_height, 0.0]);
    normals.push([0.0, -1.0, 0.0]);
    texcoords.push([0.5, 0.5]);
    let bot_rim = u32_sat(positions.len());
    for j in 0..n {
        let theta = two_pi * j.as_f32() / n.as_f32();
        let (s, c) = theta.sin_cos();
        positions.push([c * radius, -half_height, s * radius]);
        normals.push([0.0, -1.0, 0.0]);
        texcoords.push([0.5 + 0.5 * c, 0.5 + 0.5 * s]);
    }
    for j in 0..n {
        let r0 = bot_rim + u32_sat(j);
        let r1 = bot_rim + u32_sat((j + 1) % n);
        indices.extend_from_slice(&[bot_centre, r0, r1]);
    }

    GpuMeshDesc {
        prim_path: String::new(),
        local_aabb: aabb_of_positions(&positions),
        positions,
        normals,
        tangents: Vec::new(),
        texcoords,
        vertex_colors: Vec::new(),
        indices,
        double_sided: false,
        material_path: None,
        skin: None,
        serial: content_serial(3, &[radius, half_height, slices.as_f32()]),
    }
}

/// Generates a tessellated cone for `UsdGeomCone` prims.
///
/// Apex at `(0, +half_height, 0)`, base circle at `y = −half_height` with `radius`.
/// Side vertices carry **smooth** normals: each rim vertex uses its own radial
/// normal `N(θ) = (h·cosθ, r, h·sinθ)/L`, and every apex vertex uses the shared
/// `+Y` normal (the normalized average of all rim normals, whose horizontal
/// components cancel). Because both triangles meeting at a slant edge then agree
/// on the normal at *both* endpoints of that edge, the normal field is continuous
/// across every seam — fully smooth all the way to the tip, with no welding.
/// Base cap uses a flat −Y normal. All faces are CCW from outside. `slices` is
/// the polygon count (minimum 3).
#[must_use]
pub fn tessellate_cone(radius: f32, half_height: f32, slices: u32) -> GpuMeshDesc {
    let n = slices.max(3) as usize;
    let two_pi = 2.0 * std::f32::consts::PI;

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut texcoords: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Slant length for outward cone surface normal: N = (h·cosθ, r, h·sinθ) / L.
    let h = 2.0 * half_height;
    let slant_len = (radius * radius + h * h).sqrt();
    let radial_n = |s: f32, c: f32| [h * c / slant_len, radius / slant_len, h * s / slant_len];

    // ── Side ─────────────────────────────────────────────────────────────────
    // 3 unique vertices per fan triangle. Rim vertices use their exact per-angle
    // normal (shared angles between neighbours carry identical values); the apex
    // uses the common +Y average normal so slant-edge seams stay continuous right
    // up to the degenerate tip. Order [apex, rim[j+1], rim[j]]: CCW from outside.
    let apex_pos = [0.0_f32, half_height, 0.0];
    let apex_n = [0.0_f32, 1.0, 0.0];
    for j in 0..n {
        let theta_j = two_pi * j.as_f32() / n.as_f32();
        let theta_j1 = two_pi * (j + 1).as_f32() / n.as_f32();
        let (sj, cj) = theta_j.sin_cos();
        let (sj1, cj1) = theta_j1.sin_cos();

        let base = u32_sat(positions.len());
        positions.push(apex_pos);
        normals.push(apex_n);
        texcoords.push([0.5, 1.0]);

        positions.push([cj1 * radius, -half_height, sj1 * radius]);
        normals.push(radial_n(sj1, cj1));
        texcoords.push([(j + 1).as_f32() / n.as_f32(), 0.0]);

        positions.push([cj * radius, -half_height, sj * radius]);
        normals.push(radial_n(sj, cj));
        texcoords.push([j.as_f32() / n.as_f32(), 0.0]);

        indices.extend_from_slice(&[base, base + 1, base + 2]);
    }

    // ── Bottom cap (−Y) ──────────────────────────────────────────────────────
    // Fan [centre, rim[j], rim[j+1]]: normal = −Y ✓.
    let bot_centre = u32_sat(positions.len());
    positions.push([0.0, -half_height, 0.0]);
    normals.push([0.0, -1.0, 0.0]);
    texcoords.push([0.5, 0.5]);
    let bot_rim = u32_sat(positions.len());
    for j in 0..n {
        let theta = two_pi * j.as_f32() / n.as_f32();
        let (s, c) = theta.sin_cos();
        positions.push([c * radius, -half_height, s * radius]);
        normals.push([0.0, -1.0, 0.0]);
        texcoords.push([0.5 + 0.5 * c, 0.5 + 0.5 * s]);
    }
    for j in 0..n {
        let r0 = bot_rim + u32_sat(j);
        let r1 = bot_rim + u32_sat((j + 1) % n);
        indices.extend_from_slice(&[bot_centre, r0, r1]);
    }

    GpuMeshDesc {
        prim_path: String::new(),
        local_aabb: aabb_of_positions(&positions),
        positions,
        normals,
        tangents: Vec::new(),
        texcoords,
        vertex_colors: Vec::new(),
        indices,
        double_sided: false,
        material_path: None,
        skin: None,
        serial: content_serial(4, &[radius, half_height, slices.as_f32()]),
    }
}

/// Generates a tessellated capsule for `UsdGeomCapsule` prims.
///
/// Axis along Y: apex at `(0, half_mid + radius, 0)`, base at `(0, -(half_mid + radius), 0)`.
/// `mid_height` is the USD `height` attribute — the length of the **cylindrical midsection**
/// (not the total height).  Total height = `mid_height + 2 * radius`.
///
/// Layout: `hemi_stacks + 1` latitude rings for the top hemisphere, then `hemi_stacks + 1`
/// rings for the bottom hemisphere.  The pair of equator rings (top: `y = mid_height/2`,
/// bottom: `y = -mid_height/2`) forms the cylinder band between the two caps.
/// `slices` and `hemi_stacks` are clamped to a minimum of 3 and 2 respectively.
#[must_use]
pub fn tessellate_capsule(
    radius: f32,
    mid_height: f32,
    slices: u32,
    hemi_stacks: u32,
) -> GpuMeshDesc {
    let n = slices.max(3) as usize;
    let hs = hemi_stacks.max(2) as usize;
    let two_pi = 2.0 * std::f32::consts::PI;
    let half_pi = std::f32::consts::FRAC_PI_2;
    let half_mid = mid_height * 0.5;
    let total_half = half_mid + radius; // half of total height

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut texcoords: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Emit one latitude ring (n+1 vertices) at the given phi angle and Y offset.
    // phi in [0, PI]: sphere latitude; y_base is +half_mid or -half_mid.
    let emit_ring = |positions: &mut Vec<[f32; 3]>,
                     normals: &mut Vec<[f32; 3]>,
                     texcoords: &mut Vec<[f32; 2]>,
                     phi: f32,
                     y_base: f32| {
        let (sin_phi, cos_phi) = phi.sin_cos();
        let ring_r = radius * sin_phi;
        let ring_y = y_base + radius * cos_phi;
        // V = 0 at top apex, 1 at bottom apex (wgpu top-down convention).
        let v = (total_half - ring_y) / (2.0 * total_half);
        for j in 0..=n {
            let theta = two_pi * j.as_f32() / n.as_f32();
            let (st, ct) = theta.sin_cos();
            positions.push([ring_r * ct, ring_y, ring_r * st]);
            normals.push([sin_phi * ct, cos_phi, sin_phi * st]);
            texcoords.push([j.as_f32() / n.as_f32(), v]);
        }
    };

    // Top hemisphere: phi from 0 (apex) to PI/2 (equator at y = +half_mid).
    for i in 0..=hs {
        let phi = half_pi * i.as_f32() / hs.as_f32();
        emit_ring(&mut positions, &mut normals, &mut texcoords, phi, half_mid);
    }
    // Bottom hemisphere: phi from PI/2 (equator at y = -half_mid) to PI (apex).
    for i in 0..=hs {
        let phi = half_pi + half_pi * i.as_f32() / hs.as_f32();
        emit_ring(&mut positions, &mut normals, &mut texcoords, phi, -half_mid);
    }

    // Indices: same CCW pattern as the sphere fix — [a, a+1, b, b, a+1, b+1].
    // The pair at ring = hs (top equator → bottom equator) forms the cylinder band.
    let total_rings = 2 * (hs + 1); // hs+1 top + hs+1 bottom
    for ring in 0..total_rings - 1 {
        for j in 0..n {
            let a = u32_sat(ring * (n + 1) + j);
            let b = u32_sat((ring + 1) * (n + 1) + j);
            indices.extend_from_slice(&[a, a + 1, b, b, a + 1, b + 1]);
        }
    }

    GpuMeshDesc {
        prim_path: String::new(),
        local_aabb: aabb_of_positions(&positions),
        positions,
        normals,
        tangents: Vec::new(),
        texcoords,
        vertex_colors: Vec::new(),
        indices,
        double_sided: false,
        material_path: None,
        skin: None,
        serial: content_serial(
            5,
            &[radius, mid_height, slices.as_f32(), hemi_stacks.as_f32()],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Winding helpers ───────────────────────────────────────────────────────

    /// Returns the signed (unnormalised) normal of a triangle via (v1-v0)×(v2-v0).
    fn tri_normal(p: &[[f32; 3]], i0: u32, i1: u32, i2: u32) -> Vec3 {
        let v0 = Vec3::from(p[i0 as usize]);
        let v1 = Vec3::from(p[i1 as usize]);
        let v2 = Vec3::from(p[i2 as usize]);
        (v1 - v0).cross(v2 - v0)
    }

    /// Centroid of a triangle.
    fn tri_centroid(p: &[[f32; 3]], i0: u32, i1: u32, i2: u32) -> Vec3 {
        let v0 = Vec3::from(p[i0 as usize]);
        let v1 = Vec3::from(p[i1 as usize]);
        let v2 = Vec3::from(p[i2 as usize]);
        (v0 + v1 + v2) / 3.0
    }

    /// Assert every non-degenerate triangle in `mesh` has an outward-facing normal
    /// (positive dot product between the cross-product normal and the vector from
    /// `origin` to the triangle centroid).  Verifies CCW exterior winding for any
    /// convex mesh centred at `origin`.
    fn assert_all_outward(mesh: &GpuMeshDesc, origin: Vec3) {
        for (tri_idx, tri) in mesh.indices.chunks_exact(3).enumerate() {
            let [i0, i1, i2] = [tri[0], tri[1], tri[2]];
            let n = tri_normal(&mesh.positions, i0, i1, i2);
            if n.length_squared() < 1e-10 {
                continue; // skip degenerate (e.g. sphere pole triangles)
            }
            let out = tri_centroid(&mesh.positions, i0, i1, i2) - origin;
            let dot = n.dot(out);
            assert!(
                dot > 0.0,
                "triangle {tri_idx} [{i0},{i1},{i2}] is CW from outside (dot={dot:.4})"
            );
        }
    }

    // ── Cube ──────────────────────────────────────────────────────────────────

    #[test]
    fn tessellate_serials_are_content_stable() {
        // Re-tessellating the *same* shape must yield the *same* serial, so the
        // renderer's per-key mesh cache does not evict + re-upload a static gizmo
        // every frame (B3). A parameter change must still change the serial.
        assert_eq!(
            tessellate_cube(0.5).serial,
            tessellate_cube(0.5).serial,
            "identical cube params must produce a stable serial"
        );
        assert_ne!(
            tessellate_cube(0.5).serial,
            tessellate_cube(0.25).serial,
            "a changed half-extent must change the serial"
        );
        assert_eq!(
            tessellate_sphere(1.0, 8, 16).serial,
            tessellate_sphere(1.0, 8, 16).serial
        );
        assert_eq!(
            tessellate_cylinder(0.2, 0.5, 12).serial,
            tessellate_cylinder(0.2, 0.5, 12).serial
        );
        assert_eq!(
            tessellate_cone(0.2, 0.5, 12).serial,
            tessellate_cone(0.2, 0.5, 12).serial
        );
        assert_eq!(
            tessellate_capsule(0.01, 0.02, 8, 2).serial,
            tessellate_capsule(0.01, 0.02, 8, 2).serial
        );
        // Different shape kinds with identical params should not alias (the
        // per-kind tag keeps collisions rare).
        assert_ne!(
            tessellate_cylinder(0.2, 0.5, 12).serial,
            tessellate_cone(0.2, 0.5, 12).serial,
            "cylinder and cone with identical params must differ by kind tag"
        );
    }

    #[test]
    fn cube_geometry_counts() {
        let cube = tessellate_cube(0.5);
        assert_eq!(cube.vertex_count(), 24); // 6 faces × 4 verts
        assert_eq!(cube.triangle_count(), 12); // 6 faces × 2 tris
        assert_eq!(cube.normals.len(), cube.positions.len());
    }

    #[test]
    fn cube_winding_ccw_outward() {
        let cube = tessellate_cube(0.5);
        assert_all_outward(&cube, Vec3::ZERO);
    }

    #[test]
    fn cube_normals_outward() {
        // compute_flat_normals must agree with assert_all_outward after the winding fix.
        let cube = tessellate_cube(1.0);
        for n in &cube.normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-4, "cube normal not unit: {n:?}");
        }
    }

    // ── Plane ─────────────────────────────────────────────────────────────────

    #[test]
    fn plane_geometry_counts() {
        let plane = tessellate_plane(2.0, 4.0, GprimAxis::Z);
        assert_eq!(plane.vertex_count(), 4); // single quad
        assert_eq!(plane.triangle_count(), 2);
        assert_eq!(plane.normals.len(), plane.positions.len());
        assert!(plane.double_sided, "a UsdGeomPlane is double-sided");
    }

    #[test]
    fn plane_serial_is_content_stable_per_axis() {
        assert_eq!(
            tessellate_plane(2.0, 4.0, GprimAxis::Z).serial,
            tessellate_plane(2.0, 4.0, GprimAxis::Z).serial
        );
        // A changed axis (same width/length) must change the serial …
        assert_ne!(
            tessellate_plane(2.0, 4.0, GprimAxis::Z).serial,
            tessellate_plane(2.0, 4.0, GprimAxis::X).serial
        );
        // … and so must a changed extent.
        assert_ne!(
            tessellate_plane(2.0, 4.0, GprimAxis::Z).serial,
            tessellate_plane(2.0, 5.0, GprimAxis::Z).serial
        );
    }

    #[test]
    fn plane_normal_points_along_axis() {
        // The flat normal (from winding) must point along +axis for every axis.
        for (axis, expected) in [
            (GprimAxis::X, Vec3::X),
            (GprimAxis::Y, Vec3::Y),
            (GprimAxis::Z, Vec3::Z),
        ] {
            let plane = tessellate_plane(2.0, 4.0, axis);
            let n = tri_normal(&plane.positions, 0, 1, 2).normalize();
            assert!(
                (n - expected).length() < 1e-5,
                "axis {axis:?}: normal {n:?} != {expected:?}"
            );
        }
    }

    #[test]
    fn plane_width_length_land_on_correct_axes() {
        // Per the UsdGeomPlane schema: X → width Z / length Y, Y → width X /
        // length Z, Z → width X / length Y. width=2 (half 1), length=6 (half 3).
        let span = |plane: &GpuMeshDesc, comp: usize| -> f32 {
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for p in &plane.positions {
                lo = lo.min(p[comp]);
                hi = hi.max(p[comp]);
            }
            hi - lo
        };
        // (axis, width-component, length-component)
        for (axis, w_comp, l_comp) in [
            (GprimAxis::X, 2usize, 1usize), // Z, Y
            (GprimAxis::Y, 0usize, 2usize), // X, Z
            (GprimAxis::Z, 0usize, 1usize), // X, Y
        ] {
            let plane = tessellate_plane(2.0, 6.0, axis);
            assert!(
                (span(&plane, w_comp) - 2.0).abs() < 1e-5,
                "axis {axis:?}: width should span 2 on component {w_comp}"
            );
            assert!(
                (span(&plane, l_comp) - 6.0).abs() < 1e-5,
                "axis {axis:?}: length should span 6 on component {l_comp}"
            );
        }
    }

    // ── Sphere ────────────────────────────────────────────────────────────────

    #[test]
    fn sphere_geometry_non_degenerate() {
        let sphere = tessellate_sphere(1.0, 8, 16);
        assert!(!sphere.positions.is_empty());
        assert_eq!(sphere.normals.len(), sphere.positions.len());
        for n in &sphere.normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-4, "normal not unit: {len}");
        }
    }

    #[test]
    fn sphere_winding_ccw_outward() {
        let sphere = tessellate_sphere(1.0, 8, 16);
        assert_all_outward(&sphere, Vec3::ZERO);
    }

    // ── Cylinder ─────────────────────────────────────────────────────────────

    #[test]
    fn cylinder_geometry_counts() {
        let n: u32 = 16;
        let cyl = tessellate_cylinder(1.0, 0.5, n);
        let n = n as usize;
        // side: 2n verts, top cap: 1+n, bottom cap: 1+n → total 4n+2
        assert_eq!(cyl.vertex_count(), 4 * n + 2);
        // side: 2n tris, top cap: n, bottom cap: n → total 4n
        assert_eq!(cyl.triangle_count(), 4 * n);
        assert_eq!(cyl.normals.len(), cyl.positions.len());
        assert_eq!(cyl.texcoords.len(), cyl.positions.len());
    }

    #[test]
    fn cylinder_winding_ccw_outward() {
        let cyl = tessellate_cylinder(1.0, 0.5, 16);
        assert_all_outward(&cyl, Vec3::ZERO);
    }

    #[test]
    fn cylinder_cap_normals_flat() {
        let cyl = tessellate_cylinder(1.0, 1.0, 8);
        // All cap normals are either exactly +Y or -Y.
        let top_centre = (8 * 2) as usize; // after 2*n side verts
        for k in top_centre..top_centre + 1 + 8 {
            let n = cyl.normals[k];
            assert!((n[1] - 1.0).abs() < 1e-6, "top cap normal not +Y: {n:?}");
        }
        let bot_centre = top_centre + 1 + 8;
        for k in bot_centre..bot_centre + 1 + 8 {
            let n = cyl.normals[k];
            assert!((n[1] + 1.0).abs() < 1e-6, "bot cap normal not -Y: {n:?}");
        }
    }

    // ── Cone ─────────────────────────────────────────────────────────────────

    #[test]
    fn cone_geometry_counts() {
        let n: u32 = 12;
        let cone = tessellate_cone(1.0, 0.5, n);
        let n = n as usize;
        // side: 3n verts (per-face), base cap: 1+n → total 4n+1
        assert_eq!(cone.vertex_count(), 4 * n + 1);
        // side: n tris, base cap: n → total 2n
        assert_eq!(cone.triangle_count(), 2 * n);
        assert_eq!(cone.normals.len(), cone.positions.len());
    }

    #[test]
    fn cone_winding_ccw_outward() {
        let cone = tessellate_cone(1.0, 0.5, 16);
        assert_all_outward(&cone, Vec3::ZERO);
    }

    #[test]
    fn cone_base_normal_minus_y() {
        let n: u32 = 8;
        let cone = tessellate_cone(1.0, 1.0, n);
        let n = n as usize;
        let cap_start = 3 * n; // after 3n side verts
        for k in cap_start..cap_start + 1 + n {
            let nm = cone.normals[k];
            assert!(
                (nm[1] + 1.0).abs() < 1e-6,
                "cone base normal not -Y: {nm:?}"
            );
        }
    }

    #[test]
    fn cone_side_normals_outward_y_positive() {
        // Cone side normals must have positive Y (slopes outward AND upward).
        let cone = tessellate_cone(1.0, 1.0, 16);
        let n = 16_usize;
        for k in 0..3 * n {
            assert!(
                cone.normals[k][1] > 0.0,
                "cone side normal Y not positive at vertex {k}: {:?}",
                cone.normals[k]
            );
        }
    }

    #[test]
    fn cone_side_normals_are_smooth_radial() {
        // The side must NOT be flat-shaded: rim vertices vary radially, and the
        // shared rim angle between adjacent triangles must carry an identical
        // normal so the circumferential seam interpolates smoothly.
        let n = 16_usize;
        let cone = tessellate_cone(1.0, 1.0, u32_sat(n));
        // Triangle j occupies vertices [3j, 3j+1, 3j+2] = [apex, rim_{j+1}, rim_j].
        // Within a triangle the two rim normals differ (radial variation)…
        let rim_j1_tri0 = Vec3::from(cone.normals[1]);
        let rim_j_tri0 = Vec3::from(cone.normals[2]);
        assert!(
            (rim_j1_tri0 - rim_j_tri0).length() > 1e-3,
            "side is still flat: the two rim normals of a triangle are identical"
        );
        // …and triangle 0's rim_{j+1} (vertex 1) equals triangle 1's rim_j
        // (vertex 3*1+2 = 5): the shared angle θ=1·2π/n carries one normal.
        let shared_from_tri0 = Vec3::from(cone.normals[1]);
        let shared_from_tri1 = Vec3::from(cone.normals[5]);
        assert!(
            (shared_from_tri0 - shared_from_tri1).length() < 1e-5,
            "shared rim angle must carry an identical smooth normal (no facet)"
        );
        // Every apex vertex (indices 3j) must carry the SAME normal (+Y), so both
        // triangles meeting at a slant edge agree at the apex endpoint too — this
        // is what removes the facet seams that otherwise widen toward the tip.
        for j in 0..n {
            let apex = Vec3::from(cone.normals[3 * j]);
            assert!(
                (apex - Vec3::Y).length() < 1e-6,
                "apex normal {j} must be +Y for seam continuity: {apex:?}"
            );
        }
    }

    // ── Capsule ───────────────────────────────────────────────────────────────

    #[test]
    fn capsule_geometry_counts() {
        let n: usize = 16;
        let hs: usize = 4;
        let cap = tessellate_capsule(1.0, 2.0, u32_sat(n), u32_sat(hs));
        // total rings = 2*(hs+1), verts per ring = n+1
        assert_eq!(cap.vertex_count(), 2 * (hs + 1) * (n + 1));
        // quads per ring pair = n, pairs = 2*(hs+1)-1, each quad = 2 tris
        assert_eq!(cap.triangle_count(), (2 * (hs + 1) - 1) * n * 2);
        assert_eq!(cap.normals.len(), cap.positions.len());
        assert_eq!(cap.texcoords.len(), cap.positions.len());
    }

    #[test]
    fn capsule_winding_ccw_outward() {
        let cap = tessellate_capsule(1.0, 2.0, 16, 4);
        assert_all_outward(&cap, Vec3::ZERO);
    }

    #[test]
    fn capsule_zero_mid_height_is_sphere() {
        // mid_height=0 → capsule degenerates to a sphere; winding still holds.
        let cap = tessellate_capsule(1.0, 0.0, 16, 4);
        assert_all_outward(&cap, Vec3::ZERO);
    }

    #[test]
    fn capsule_normals_unit_length() {
        let cap = tessellate_capsule(0.5, 1.0, 12, 3);
        for nm in &cap.normals {
            let len = (nm[0] * nm[0] + nm[1] * nm[1] + nm[2] * nm[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-4, "capsule normal not unit: {nm:?}");
        }
    }

    // ── Smooth normals (no authored normals) ───────────────────────────────────

    /// A unit cube authored as a bare polygon soup (8 points, 6 quads), no
    /// normals. Every one of the 8 vertices is a 3-face corner with 90° dihedrals,
    /// so smooth synthesis must keep each face-corner's normal axis-aligned (hard
    /// edges), NOT round them toward the body diagonal.
    fn cube_polygon_soup() -> (Vec<[f32; 3]>, Vec<i32>, Vec<i32>) {
        let h = 0.5;
        let points = vec![
            [-h, -h, -h],
            [h, -h, -h],
            [h, h, -h],
            [-h, h, -h],
            [-h, -h, h],
            [h, -h, h],
            [h, h, h],
            [-h, h, h],
        ];
        // 6 quads, CCW when viewed from outside (matching tessellate_cube faces).
        let faces: [[i32; 4]; 6] = [
            [0, 3, 2, 1], // -Z
            [5, 6, 7, 4], // +Z
            [4, 7, 3, 0], // -X
            [1, 2, 6, 5], // +X
            [3, 7, 6, 2], // +Y
            [0, 1, 5, 4], // -Y
        ];
        let counts = vec![4_i32; 6];
        let indices: Vec<i32> = faces.iter().flatten().copied().collect();
        (points, counts, indices)
    }

    #[test]
    fn smooth_normals_keep_cube_edges_hard() {
        let (points, counts, indices) = cube_polygon_soup();
        let normals =
            compute_smooth_normals(&points, &counts, &indices, DEFAULT_CREASE_ANGLE, false);
        // 6 quads × 4 corners.
        assert_eq!(normals.len(), 24);
        // Every corner normal must be axis-aligned (one component ±1, others ~0):
        // if the 90° cube edges were smoothed, corners would read ~(±0.577)³.
        for n in &normals {
            let max = n[0].abs().max(n[1].abs()).max(n[2].abs());
            assert!(
                max > 0.999,
                "cube corner normal was rounded (not axis-aligned): {n:?}"
            );
        }
    }

    #[test]
    fn smooth_normals_face_corners_point_outward() {
        // The -Z face (first quad) must carry a -Z normal, not +Z.
        let (points, counts, indices) = cube_polygon_soup();
        let normals =
            compute_smooth_normals(&points, &counts, &indices, DEFAULT_CREASE_ANGLE, false);
        let n0 = Vec3::from(normals[0]);
        assert!(
            (n0 - Vec3::NEG_Z).length() < 1e-4,
            "-Z face normal wrong: {n0:?}"
        );
    }

    #[test]
    fn smooth_normals_weld_flat_grid() {
        // A flat 2×1 quad strip in the XZ plane: all faces coplanar, so every
        // corner — including the two shared middle points — must average to +Y.
        let points = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [2.0, 0.0, 1.0],
        ];
        // CCW from +Y above: (0,3,4,1) and (1,4,5,2).
        let counts = vec![4, 4];
        let indices = vec![0, 3, 4, 1, 1, 4, 5, 2];
        let normals =
            compute_smooth_normals(&points, &counts, &indices, DEFAULT_CREASE_ANGLE, false);
        for n in &normals {
            assert!(
                (Vec3::from(*n) - Vec3::Y).length() < 1e-4,
                "flat grid normal not +Y: {n:?}"
            );
        }
    }

    #[test]
    fn smooth_normals_smooth_a_gentle_fold() {
        // Two quads folded by 20° (< 60° crease) share a smooth edge: the two
        // corners on the shared edge must blend (not equal either face normal).
        let a = 20.0_f32.to_radians();
        let points = vec![
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0], // flat quad in XZ
            [1.0, 0.0, 1.0],
            [1.0 + a.sin(), a.sin(), 0.0], // folded up by ~20°
            [1.0 + a.sin(), a.sin(), 1.0],
        ];
        // quad A: (0,1,3,2) normal ~+Y; quad B: (2,3,5,4) tilted.
        let counts = vec![4, 4];
        let indices = vec![0, 1, 3, 2, 2, 3, 5, 4];
        let normals =
            compute_smooth_normals(&points, &counts, &indices, DEFAULT_CREASE_ANGLE, false);
        // Corner at shared point 2 in quad A is index 3; in quad B index 4.
        let shared_a = Vec3::from(normals[3]);
        let shared_b = Vec3::from(normals[4]);
        assert!(
            (shared_a - shared_b).length() < 1e-4,
            "shared smooth-edge corners should have identical welded normals"
        );
        // And the welded normal must be tilted off pure +Y (blended).
        assert!(
            shared_a.x < -1e-3,
            "welded normal should lean away from +Y: {shared_a:?}"
        );
    }

    #[test]
    fn smooth_normals_determinism() {
        let (points, counts, indices) = cube_polygon_soup();
        let a = compute_smooth_normals(&points, &counts, &indices, DEFAULT_CREASE_ANGLE, false);
        let b = compute_smooth_normals(&points, &counts, &indices, DEFAULT_CREASE_ANGLE, false);
        assert_eq!(a, b, "smooth normal synthesis must be deterministic");
    }

    #[test]
    fn smooth_normals_left_handed_negates() {
        let (points, counts, indices) = cube_polygon_soup();
        let rh = compute_smooth_normals(&points, &counts, &indices, DEFAULT_CREASE_ANGLE, false);
        let lh = compute_smooth_normals(&points, &counts, &indices, DEFAULT_CREASE_ANGLE, true);
        for (r, l) in rh.iter().zip(lh.iter()) {
            assert!(
                (Vec3::from(*r) + Vec3::from(*l)).length() < 1e-4,
                "left-handed normals must be negated: {r:?} vs {l:?}"
            );
        }
    }

    // ── Crease / corner overrides (Phase 2) ─────────────────────────────────────

    #[test]
    fn crease_edge_forces_hard_split() {
        // The gentle 20° fold would normally smooth; an authored crease on the
        // shared edge (points 2–3) must force it hard so each face keeps its own.
        let a = 20.0_f32.to_radians();
        let points = vec![
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 1.0],
            [1.0 + a.sin(), a.sin(), 0.0],
            [1.0 + a.sin(), a.sin(), 1.0],
        ];
        let counts = vec![4, 4];
        let indices = vec![0, 1, 3, 2, 2, 3, 5, 4];
        let mut hard = HashSet::new();
        hard.insert((2_u32, 3_u32));
        let normals = compute_smooth_normals_with_creases(
            &points,
            &counts,
            &indices,
            DEFAULT_CREASE_ANGLE,
            false,
            &hard,
            &HashSet::new(),
        );
        let shared_a = Vec3::from(normals[3]);
        let shared_b = Vec3::from(normals[4]);
        assert!(
            (shared_a - shared_b).length() > 1e-2,
            "authored crease must keep the shared edge hard"
        );
    }

    #[test]
    fn hard_edge_on_smooth_cycle_is_not_bypassed() {
        // A closed octagonal tube: 8 side quads whose shared vertical edges are
        // shallow 45° folds (smooth under the 60° default). The faces form a
        // CYCLE, so a global per-face union would merge all 8 into one smoothing
        // group — letting a single authored hard edge be bypassed through the
        // other 7 smooth edges and averaging its seam normals. Per-point grouping
        // must keep that one authored edge hard.
        use std::f32::consts::TAU;
        let n: usize = 8;
        let nn = u8::try_from(n).unwrap();
        let mut points = Vec::new();
        for i in 0..nn {
            let a = TAU * f32::from(i) / f32::from(nn);
            points.push([a.cos(), 0.0, a.sin()]); // bottom ring: index i
        }
        for i in 0..nn {
            let a = TAU * f32::from(i) / f32::from(nn);
            points.push([a.cos(), 1.0, a.sin()]); // top ring: index n + i
        }
        let mut counts = Vec::new();
        let mut indices = Vec::new();
        for i in 0..nn {
            let j = (i + 1) % nn;
            counts.push(4);
            // quad corners: bottom i, bottom j, top j, top i
            indices.push(i32::from(i));
            indices.push(i32::from(j));
            indices.push(i32::from(nn + j));
            indices.push(i32::from(nn + i));
        }
        // Author the vertical edge at ring index 0 (points 0 ↔ n) as hard. It is
        // shared only by face 0 and face n-1; every other edge stays smooth.
        let mut hard = HashSet::new();
        hard.insert((0u32, u32::from(nn)));
        let normals = compute_smooth_normals_with_creases(
            &points,
            &counts,
            &indices,
            DEFAULT_CREASE_ANGLE,
            false,
            &hard,
            &HashSet::new(),
        );

        // Corner layout: face f emits 4 corners at base f*4 in order
        // [bottom i, bottom j, top j, top i]. The hard seam's point 0 is face 0's
        // corner 0 and face (n-1)'s corner 1 (base (n-1)*4 + 1).
        let seam_face0 = Vec3::from(normals[0]);
        let seam_face_last = Vec3::from(normals[(n - 1) * 4 + 1]);
        assert!(
            (seam_face0 - seam_face_last).length() > 1e-2,
            "authored hard edge on a smooth cycle must not be averaged away \
             (face0 {seam_face0:?} vs face{} {seam_face_last:?})",
            n - 1
        );

        // Sanity: a neighbouring SMOOTH vertical edge (points 1 ↔ n+1, shared by
        // face 0 and face 1) must still share one normal at point 1 — face 0's
        // corner 1 and face 1's corner 0 (base 1*4).
        let smooth_face0 = Vec3::from(normals[1]);
        let smooth_face1 = Vec3::from(normals[4]);
        assert!(
            (smooth_face0 - smooth_face1).length() < 1e-5,
            "a smooth shared edge must keep one averaged normal \
             ({smooth_face0:?} vs {smooth_face1:?})"
        );
    }

    #[test]
    fn hard_edges_from_creases_respects_sharpness() {
        // One crease chain 0→1→2 (two edges); only the second is sharp enough.
        let edges = hard_edges_from_creases(&[0, 1, 2], &[3], &[0.5, 10.0], 1.0);
        assert!(!edges.contains(&(0, 1)), "soft crease edge excluded");
        assert!(edges.contains(&(1, 2)), "sharp crease edge included");
    }

    #[test]
    fn corner_point_set_respects_sharpness() {
        let set = corner_point_set(&[4, 7], &[0.0, 10.0], 1.0);
        assert!(!set.contains(&4));
        assert!(set.contains(&7));
    }

    // ── from_mesh_comp integration (authored-normals regression + fallback) ─────

    fn mesh_comp_from(
        points: Vec<[f32; 3]>,
        counts: Vec<i32>,
        indices: Vec<i32>,
        normals: Vec<[f32; 3]>,
    ) -> MeshComp {
        MeshComp {
            geometry_version: 1,
            points,
            normals,
            tangents: Vec::new(),
            texcoords: Vec::new(),
            face_vertex_counts: counts,
            face_vertex_indices: indices,
            display_colors: Vec::new(),
            material_path: None,
            material_subsets: Vec::new(),
            left_handed: false,
            double_sided: false,
            prototype_only: false,
            subdivision_scheme: String::new(),
            crease_indices: Vec::new(),
            crease_lengths: Vec::new(),
            crease_sharpnesses: Vec::new(),
            corner_indices: Vec::new(),
            corner_sharpnesses: Vec::new(),
        }
    }

    #[test]
    fn from_mesh_comp_passes_through_authored_normals() {
        // A single quad with authored (face-corner) normals must be used verbatim —
        // the synthesis path must never override authored data (hard-edge assets).
        let points = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let authored = vec![[0.0, 0.0, 1.0]; 4];
        let mc = mesh_comp_from(points, vec![4], vec![0, 1, 2, 3], authored.clone());
        let gpu = GpuMeshDesc::from_mesh_comp("/m", &mc);
        assert_eq!(gpu.normals, authored, "authored normals must pass through");
    }

    #[test]
    fn from_mesh_comp_synthesizes_smooth_when_absent() {
        // Same cube, no authored normals → synthesized, and still hard-edged.
        let (points, counts, indices) = cube_polygon_soup();
        let mc = mesh_comp_from(points, counts, indices, Vec::new());
        let gpu = GpuMeshDesc::from_mesh_comp("/cube", &mc);
        assert_eq!(gpu.normals.len(), gpu.positions.len());
        for n in &gpu.normals {
            let max = n[0].abs().max(n[1].abs()).max(n[2].abs());
            assert!(max > 0.999, "synthesized cube normal not axis-aligned: {n:?}");
        }
    }

    #[test]
    fn display_color_ignored_when_material_bound() {
        // Regression: an exporter placeholder `displayColor = (0,0,0)` on a mesh that
        // has a bound surface material must NOT zero the albedo. USD/Hydra ignore
        // displayColor when a material governs the colour; otherwise the shader's
        // `base_color * vertex_color` tint renders the whole textured mesh black
        // (observed on RealityKit/Apple `.usdz` exports such as toy_car.usdz).
        let points = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let mut mc = mesh_comp_from(points, vec![4], vec![0, 1, 2, 3], vec![[0.0, 0.0, 1.0]; 4]);
        mc.display_colors = vec![[0.0, 0.0, 0.0]; 4];
        mc.material_path = Some("/World/Looks/mat".to_owned());
        let gpu = GpuMeshDesc::from_mesh_comp("/m", &mc);
        assert_eq!(
            gpu.vertex_colors,
            vec![[1.0_f32, 1.0, 1.0]; gpu.positions.len()],
            "bound-material mesh must use the identity white tint, not authored displayColor"
        );
    }

    #[test]
    fn display_color_preserved_without_material() {
        // Fallback behaviour unchanged: with no bound material, authored displayColor
        // is the gprim's colour and must flow through to the vertex-colour tint.
        let points = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let mut mc = mesh_comp_from(points, vec![4], vec![0, 1, 2, 3], vec![[0.0, 0.0, 1.0]; 4]);
        mc.display_colors = vec![[0.25, 0.5, 0.75]; 4];
        mc.material_path = None;
        let gpu = GpuMeshDesc::from_mesh_comp("/m", &mc);
        assert_eq!(
            gpu.vertex_colors,
            vec![[0.25_f32, 0.5, 0.75]; gpu.positions.len()],
            "unmaterialed mesh must keep its authored displayColor"
        );
    }

    #[test]
    fn display_color_ignored_when_only_a_subset_is_bound() {
        // RealityKit/Apple exports commonly leave the mesh unbound and bind a single
        // `partition`-family `UsdGeomSubset` over every face. That subset binding still
        // governs the colour, so displayColor must be ignored just as for a whole-mesh
        // binding — otherwise toy_car.usdz (mesh `material_path = None`, one bound subset,
        // displayColor = (0,0,0)) renders black.
        let points = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let mut mc = mesh_comp_from(points, vec![4], vec![0, 1, 2, 3], vec![[0.0, 0.0, 1.0]; 4]);
        mc.display_colors = vec![[0.0, 0.0, 0.0]; 4];
        mc.material_path = None;
        mc.material_subsets = vec![idtx_usd::MaterialSubsetRecord {
            material_path: Some("/World/Looks/mat".to_owned()),
            face_indices: vec![0],
        }];
        let gpu = GpuMeshDesc::from_mesh_comp("/m", &mc);
        assert_eq!(
            gpu.vertex_colors,
            vec![[1.0_f32, 1.0, 1.0]; gpu.positions.len()],
            "subset-bound mesh must use the identity white tint, not authored displayColor"
        );
    }

    #[test]
    fn synthesized_normals_keyed_by_content_not_version() {
        // Two meshes at the SAME prim path with the SAME geometry_version but
        // DIFFERENT geometry — the cross-session collision case: geometry_version
        // restarts at 1 per hydrated world, so two open stages share this key.
        // The content fingerprint must distinguish them so neither receives the
        // other's (possibly wrong-length) normals.
        let path = "/cache_test/content_keyed_prim";
        let (points, counts, indices) = cube_polygon_soup();
        let mut mesh_a = mesh_comp_from(points.clone(), counts.clone(), indices.clone(), Vec::new());
        mesh_a.geometry_version = 1;
        let a = synthesized_normals_cached(path, &mesh_a);

        // A single triangle at the same path + version → different corner count.
        let mut mesh_b = mesh_comp_from(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![3],
            vec![0, 1, 2],
            Vec::new(),
        );
        mesh_b.geometry_version = 1;
        let b = synthesized_normals_cached(path, &mesh_b);
        assert_ne!(a, b, "same path+version must not share normals across content");
        assert_eq!(b.len(), 3, "b must get its own normals, not a's cached vector");

        // Re-querying the original content returns a consistent result (and the
        // fingerprint ignores geometry_version — a bumped version still hits).
        let mut mesh_a_bumped = mesh_comp_from(points, counts, indices, Vec::new());
        mesh_a_bumped.geometry_version = 99;
        assert_eq!(synthesized_normals_cached(path, &mesh_a_bumped), a);
    }
}
