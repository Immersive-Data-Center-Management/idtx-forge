//! Pixel regressions for skeletal highlights. Run with a GPU adapter:
//! `cargo test -p idtx-render --test highlight_skinning -- --ignored`

use std::sync::Arc;

use glam::{Mat4, Vec3};
use idtx_render::{
    FrameTarget, RenderWorldSnapshot, Renderer,
    camera_fit::fit_camera_to_aabb,
    material::fallback_material,
    mesh::{GpuMeshDesc, SkinData, next_mesh_serial, tessellate_cube},
    scene_extract::RenderMeshInstance,
    vertex::SkinVertex,
};
use idtx_usd::RenderPurpose;

const PATH: &str = "/World/Cat/Body";
const SIZE: u32 = 256;

fn instance(key: &str, mesh: GpuMeshDesc, world_matrix: Mat4) -> RenderMeshInstance {
    let mut material = fallback_material("/Looks/White");
    material.emissive = [1.0; 3];
    RenderMeshInstance {
        prim_path: key.into(),
        mesh: Arc::new(mesh),
        world_matrix,
        material,
        purpose: RenderPurpose::Default,
        casts_shadow: false,
    }
}

fn snapshot() -> RenderWorldSnapshot {
    RenderWorldSnapshot {
        camera: Some(fit_camera_to_aabb(Vec3::splat(-2.0), Vec3::splat(2.0), 1.0)),
        ..Default::default()
    }
}

/// Compare the highlight against the beauty pass's rasterized silhouette, rather
/// than duplicating skinning math in an expected-position helper. An opaque red
/// tint must cover exactly that silhouette; the yellow outline must stay outside
/// it and within the requested six-pixel band.
fn assert_highlight_matches_beauty(
    renderer: &mut Renderer,
    target: &FrameTarget,
    snapshot: &mut RenderWorldSnapshot,
) -> Vec<bool> {
    snapshot.highlight_tints.clear();
    snapshot.highlight_outlines.clear();
    renderer.render(target, snapshot, false);
    let beauty = renderer.read_pixels(target).expect("beauty readback");
    let silhouette: Vec<bool> = beauty
        .chunks_exact(4)
        .map(|p| p[0] > 20 || p[1] > 20 || p[2] > 20)
        .collect();
    assert!(silhouette.iter().filter(|&&v| v).count() > 200);

    snapshot
        .highlight_tints
        .insert(PATH.into(), [1.0, 0.0, 0.0, 1.0]);
    snapshot
        .highlight_outlines
        .push((PATH.into(), [1.0, 1.0, 0.0, 1.0], 6.0));
    renderer.render_with_overlay(target, snapshot, None, &[], false);
    let highlighted = renderer.read_pixels(target).expect("highlight readback");
    let mut mismatch = 0;
    let mut outline_pixels = 0;
    let mut misplaced_outline = 0;
    let size = i32::try_from(SIZE).unwrap();
    for (i, p) in highlighted.chunks_exact(4).enumerate() {
        let tinted = p[0] > 200 && p[1] < 10 && p[2] < 10;
        mismatch += usize::from(tinted != silhouette[i]);
        if p[0] > 100 && p[1] > 100 && p[2] < 10 {
            outline_pixels += 1;
            let x = i32::try_from(i % SIZE as usize).unwrap();
            let y = i32::try_from(i / SIZE as usize).unwrap();
            let near_surface = (-6_i32..=6).any(|dy| {
                (-6_i32..=6).any(|dx| {
                    let (nx, ny) = (x + dx, y + dy);
                    dx * dx + dy * dy <= 36
                        && (0..size).contains(&nx)
                        && (0..size).contains(&ny)
                        && silhouette[usize::try_from(ny * size + nx).unwrap()]
                })
            });
            misplaced_outline += usize::from(silhouette[i] || !near_surface);
        }
    }
    // Single-sample beauty and mask use the same geometry. Allow a few edge
    // pixels for backend-specific floating-point association/rounding.
    assert!(
        mismatch <= 8,
        "tint missed the beauty silhouette at {mismatch} pixels"
    );
    assert!(outline_pixels > 100, "outline must be visible");
    assert!(
        misplaced_outline <= 8,
        "outline detached at {misplaced_outline} pixels"
    );
    silhouette
}

#[test]
#[ignore = "requires a wgpu GPU adapter"]
fn gpu_skinned_highlights_follow_joint_animation_and_subset_pieces() {
    let mut renderer = Renderer::new_with_sample_count(1).expect("GPU renderer");
    let mut target = renderer.create_frame_target(SIZE, SIZE);
    target.clear_color = wgpu::Color::BLACK;
    let mut mesh = tessellate_cube(0.5);
    mesh.prim_path = PATH.into();
    mesh.double_sided = true;
    mesh.skin = Some(SkinData {
        influences: mesh
            .positions
            .iter()
            .map(|p| SkinVertex {
                joints: [0, 1, 2, 3, 4, 5, 6, 7],
                // All eight slots matter, with spatially varying weights so changing
                // individual joints deforms the silhouette, rather than just moving it.
                weights: if p[1] > 0.0 {
                    [0.05, 0.05, 0.05, 0.05, 0.2, 0.2, 0.2, 0.2]
                } else {
                    [0.2, 0.2, 0.2, 0.2, 0.05, 0.05, 0.05, 0.05]
                },
            })
            .collect(),
        joint_matrices: vec![Mat4::IDENTITY.to_cols_array(); 8],
    });
    let mut snapshot = snapshot();
    for (key, faces) in [("subset_fur", [0, 2, 4]), ("remainder", [1, 3, 5])] {
        let piece = GpuMeshDesc::subset_by_faces(&mesh, &[4; 6], &faces);
        snapshot.meshes.push(instance(
            &format!("{PATH}/{key}"),
            piece,
            // Like beauty, the GPU-skinned mask must ignore this matrix; world
            // placement is already folded into the effective joint palette.
            Mat4::from_translation(Vec3::splat(100.0)),
        ));
    }
    let serials: Vec<_> = snapshot.meshes.iter().map(|i| i.mesh.serial).collect();
    let stage_basis = Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2);
    let geom_bind = Mat4::from_translation(Vec3::new(0.2, 0.0, 0.3))
        * Mat4::from_scale(Vec3::new(0.6, 1.4, 0.8));
    let mut previous: Option<Vec<bool>> = None;
    for (frame, phase) in [0.0_f32, 1.0, -0.7].into_iter().enumerate() {
        let skeleton_world = Mat4::from_translation(Vec3::new(phase, 0.1, -0.3))
            * Mat4::from_rotation_z(phase * 0.6);
        let palette: Vec<_> = (0_u16..8)
            .map(|joint| {
                let joint_pose = Mat4::from_rotation_y(phase * f32::from(joint) * 0.16)
                    * Mat4::from_translation(Vec3::new(0.0, 0.0, phase * f32::from(joint) * 0.04));
                (stage_basis * skeleton_world * joint_pose * geom_bind).to_cols_array()
            })
            .collect();
        for inst in &mut snapshot.meshes {
            Arc::make_mut(&mut inst.mesh)
                .skin
                .as_mut()
                .unwrap()
                .joint_matrices
                .clone_from(&palette);
        }
        assert_eq!(
            serials,
            snapshot
                .meshes
                .iter()
                .map(|i| i.mesh.serial)
                .collect::<Vec<_>>()
        );
        let silhouette = assert_highlight_matches_beauty(&mut renderer, &target, &mut snapshot);
        if let Some(previous) = previous {
            assert!(
                previous
                    .iter()
                    .zip(&silhouette)
                    .filter(|(a, b)| a != b)
                    .count()
                    > 100,
                "frame {frame} must visibly change the pose"
            );
        }
        previous = Some(silhouette);
    }
}

#[test]
#[ignore = "requires a wgpu GPU adapter"]
fn cpu_skinned_highlights_follow_deformed_geometry_and_skeleton_world() {
    let mut renderer = Renderer::new_with_sample_count(1).expect("GPU renderer");
    let mut target = renderer.create_frame_target(SIZE, SIZE);
    target.clear_color = wgpu::Color::BLACK;
    let mut mesh = tessellate_cube(0.5);
    mesh.prim_path = PATH.into();
    mesh.double_sided = true;
    let rest = mesh.positions.clone();
    let mut snapshot = snapshot();
    snapshot.meshes.push(instance(PATH, mesh, Mat4::IDENTITY));
    for phase in [0.0, 1.0] {
        let inst = &mut snapshot.meshes[0];
        let mesh = Arc::make_mut(&mut inst.mesh);
        // CpuOpenUsd extraction supplies skeleton-local deformed positions,
        // a fresh geometry serial, and stage_basis * skeleton_world as model.
        mesh.positions = rest
            .iter()
            .map(|p| [p[0] + phase * p[1], p[1] * (1.0 + phase), p[2] * 0.6])
            .collect();
        mesh.serial = next_mesh_serial();
        inst.world_matrix = Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2)
            * Mat4::from_translation(Vec3::new(phase, 0.3, 0.0))
            * Mat4::from_rotation_z(0.4 + phase);
        assert_highlight_matches_beauty(&mut renderer, &target, &mut snapshot);
    }
}
