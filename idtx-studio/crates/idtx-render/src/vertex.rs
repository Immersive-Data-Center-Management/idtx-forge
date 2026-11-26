//! GPU vertex layout for the scene render pipeline.

use bytemuck::{Pod, Zeroable};

/// Interleaved vertex: position, normal, UV, vertex color, tangent — 64 bytes.
///
/// `tangent` carries the world-space tangent direction (xyz) and bitangent handedness sign (w).
/// `w == 0.0` means no authored tangent was available; the fragment shader will fall back to
/// screen-space derivative TBN in that case.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub texcoord: [f32; 2],
    pub vertex_color: [f32; 3],
    _pad: f32,             // pad vertex_color to 16-byte boundary
    pub tangent: [f32; 4], // @location(4): xyz = tangent dir, w = bitangent sign (0 → SS fallback)
}

impl Vertex {
    const ATTRIBS: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
        0 => Float32x3,  // position
        1 => Float32x3,  // normal
        2 => Float32x2,  // texcoord
        3 => Float32x3,  // vertex_color
        4 => Float32x4,  // tangent (xyz + bitangent sign w)
    ];

    /// Returns the vertex buffer layout descriptor for this vertex type.
    #[must_use]
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

/// Per-vertex joint indices and weights for GPU skeletal skinning (vertex buffer slot 1).
///
/// Supports up to eight influences per vertex. The first four occupy attribute
/// locations 5 (`joints[0..4]` as `Uint32x4`) and 6 (`weights[0..4]` as
/// `Float32x4`); the second four occupy locations 7 (`joints[4..8]`) and 8
/// (`weights[4..8]`). Meshes with fewer influences zero-pad the unused slots
/// (joint 0, weight 0.0), which contribute nothing under linear blend skinning.
/// These locations match the `@location` declarations in `PBR_SKINNED_WGSL_SHADER`.
/// Location 4 is reserved for the tangent attribute in the main `Vertex` buffer (slot 0).
/// Locations 7/8 collide with `InstanceRaw`, but the skinned pipeline never binds
/// an instance buffer, so there is no conflict within a single pipeline.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SkinVertex {
    /// Joint palette indices (eight per vertex, matching `SkinBindingRecord::influences`).
    pub joints: [u32; 8],
    /// Normalized blend weights (sum ≈ 1.0; unused slots are 0.0).
    pub weights: [f32; 8],
}

impl SkinVertex {
    // Declaration order determines byte offsets (the macro packs sequentially):
    // joints[0..4]@0, joints[4..8]@16, weights[0..4]@32, weights[4..8]@48,
    // matching the `#[repr(C)]` field layout above.
    const ATTRIBS: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
        5 => Uint32x4,   // @location(5) skin_joints    (joints[0..4])
        7 => Uint32x4,   // @location(7) skin_joints_hi (joints[4..8])
        6 => Float32x4,  // @location(6) skin_weights    (weights[0..4])
        8 => Float32x4,  // @location(8) skin_weights_hi (weights[4..8])
    ];

    /// Returns the vertex buffer layout descriptor for slot 1.
    #[must_use]
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<SkinVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

/// Per-instance model matrix passed via a step-mode-Instance vertex buffer (slot 1
/// for non-skinned instanced draws).
///
/// The four columns of the column-major world matrix occupy attribute locations
/// 5–8.  The fragment shader ignores group-2 `MeshUniforms` for instanced draws;
/// the normal matrix is reconstructed from this matrix in the vertex stage.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct InstanceRaw {
    /// Column-major world matrix (four vec4 columns).
    pub model: [[f32; 4]; 4],
    /// Presence coverage (1 for legacy/native scenegraph instance batches).
    pub opacity: f32,
}

impl InstanceRaw {
    const ATTRIBS: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
        5 => Float32x4,  // @location(5) model col 0
        6 => Float32x4,  // @location(6) model col 1
        7 => Float32x4,  // @location(7) model col 2
        8 => Float32x4,  // @location(8) model col 3
        9 => Float32,
    ];

    /// Returns the vertex buffer layout descriptor for the instance buffer (slot 1).
    #[must_use]
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceRaw>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBS,
        }
    }
}

/// Converts parallel arrays into an interleaved vertex slice.
///
/// `colors` may be shorter than `positions`; missing entries default to white `[1,1,1]`.
/// `tangents` may be empty; missing entries default to `[0,0,0,0]`, which signals the
/// fragment shader to use the screen-space derivative TBN fallback.
#[must_use]
pub fn interleave(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    texcoords: &[[f32; 2]],
    colors: &[[f32; 3]],
    tangents: &[[f32; 4]],
) -> Vec<Vertex> {
    debug_assert_eq!(positions.len(), normals.len());
    debug_assert_eq!(positions.len(), texcoords.len());
    positions
        .iter()
        .zip(normals.iter())
        .zip(texcoords.iter())
        .enumerate()
        .map(|(i, ((&p, &n), &t))| {
            let vc = colors.get(i).copied().unwrap_or([1.0, 1.0, 1.0]);
            let tg = tangents.get(i).copied().unwrap_or([0.0; 4]);
            Vertex {
                position: p,
                normal: n,
                texcoord: t,
                vertex_color: vc,
                _pad: 0.0,
                tangent: tg,
            }
        })
        .collect()
}

/// Bakes a mesh's local-space vertices into world space through `model` and
/// appends them to `out`, for static draw-merge (F1/F7).
///
/// This reproduces exactly what the vertex shader does per draw
/// (`surface_gen.rs`): position is transformed by `model`, while normal **and**
/// tangent direction are transformed by the normal matrix
/// `nm = transpose(inverse(model_3x3))` and re-normalised. `tangent.w` (the
/// bitangent handedness sign, `0` = screen-space fallback) is preserved verbatim.
/// A merged batch is therefore drawn with an **identity** group-2 `MeshUniforms`
/// and produces pixel-identical output to the per-mesh draws it replaces.
///
/// `colors` may be shorter than `positions` (missing → white); `tangents` may be
/// empty (missing → `[0,0,0,0]`, signalling the SS-derivative TBN fallback).
pub fn bake_transformed_into(
    out: &mut Vec<Vertex>,
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    texcoords: &[[f32; 2]],
    colors: &[[f32; 3]],
    tangents: &[[f32; 4]],
    model: glam::Mat4,
) {
    debug_assert_eq!(positions.len(), normals.len());
    debug_assert_eq!(positions.len(), texcoords.len());
    // Normal matrix: transpose(inverse(upper-left 3×3)) — matches the shader.
    let nm = glam::Mat3::from_mat4(model).inverse().transpose();
    out.reserve(positions.len());
    for (i, (&p, &n)) in positions.iter().zip(normals.iter()).enumerate() {
        let wp = model.transform_point3(glam::Vec3::from(p)).to_array();
        let wn = (nm * glam::Vec3::from(n)).normalize_or_zero().to_array();
        let vc = colors.get(i).copied().unwrap_or([1.0, 1.0, 1.0]);
        let tg = tangents.get(i).copied().unwrap_or([0.0; 4]);
        // Transform tangent direction by `nm` too (as the shader does); preserve w.
        let wt = if tg[3] == 0.0 {
            [0.0; 4]
        } else {
            let d = (nm * glam::Vec3::new(tg[0], tg[1], tg[2])).normalize_or_zero();
            [d.x, d.y, d.z, tg[3]]
        };
        out.push(Vertex {
            position: wp,
            normal: wn,
            texcoord: *texcoords.get(i).unwrap_or(&[0.0, 0.0]),
            vertex_color: vc,
            _pad: 0.0,
            tangent: wt,
        });
    }
}
