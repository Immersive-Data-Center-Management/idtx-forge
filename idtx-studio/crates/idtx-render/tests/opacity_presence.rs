//! Native presence coverage, measured in linear HDR through the real renderer.
//! Run with `cargo test -p idtx-render --test opacity_presence -- --ignored`.
use std::sync::Arc;

use glam::{Mat4, Vec3};
use idtx_render::material::{RenderMaterialDesc, fallback_material};
use idtx_render::mesh::{GpuMeshDesc, SkinData, next_mesh_serial};
use idtx_render::scene_extract::{RenderCameraDesc, RenderInstancedBatch, RenderMeshInstance};
use idtx_render::vertex::SkinVertex;
use idtx_render::{FrameTarget, RenderWorldSnapshot, Renderer};
use idtx_usd::{AlphaPolicy, OpacityMode, RenderPurpose, SurfaceModel};

const ALPHAS: [f32; 6] = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0];
const WIDTH: u32 = 192;
const HEIGHT: u32 = 32;

#[derive(Clone, Copy, Debug)]
enum Family {
    Static,
    Instanced,
    Skinned,
    Udim,
    InstancedUdim,
}

fn quad(skin: bool) -> Arc<GpuMeshDesc> {
    Arc::new(GpuMeshDesc {
        prim_path: "/Quad".into(),
        positions: vec![
            [-0.4, -0.4, 0.0],
            [0.4, -0.4, 0.0],
            [0.4, 0.4, 0.0],
            [-0.4, 0.4, 0.0],
        ],
        normals: vec![[0.0, 0.0, 1.0]; 4],
        tangents: vec![[1.0, 0.0, 0.0, 1.0]; 4],
        texcoords: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        vertex_colors: vec![[1.0; 3]; 4],
        indices: vec![0, 1, 2, 0, 2, 3],
        double_sided: false,
        material_path: None,
        skin: skin.then(|| SkinData {
            influences: vec![
                SkinVertex {
                    joints: [0; 8],
                    weights: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
                };
                4
            ],
            joint_matrices: vec![Mat4::IDENTITY.to_cols_array()],
        }),
        serial: next_mesh_serial(),
        local_aabb: Some(([-0.4, -0.4, 0.0], [0.4, 0.4, 0.0])),
    })
}

fn patches(family: Family, textured: bool, dir: &std::path::Path) -> RenderWorldSnapshot {
    let mut snapshot = RenderWorldSnapshot {
        camera: Some(RenderCameraDesc {
            prim_path: "/Camera".into(),
            view: Mat4::from_translation(Vec3::new(0.0, 0.0, -3.0)),
            projection: Mat4::from_diagonal(glam::Vec4::new(1.0 / 3.0, 2.0, -0.1, 1.0)),
            clip_near: 0.1,
            clip_far: 10.0,
            fov_horizontal: 1.0,
            exposure_ev: 0.0,
        }),
        serial: 1,
        instance_serial: 1,
        ..Default::default()
    };
    let udim = matches!(family, Family::Udim | Family::InstancedUdim);
    for (i, alpha) in ALPHAS.into_iter().enumerate() {
        let path = format!("/Patch{i}");
        let mut material = fallback_material(format!("/Material{i}"));
        material.model = SurfaceModel::UsdPreviewSurface;
        material.opacity_mode = OpacityMode::Presence;
        material.opacity = alpha;
        material.opacity_threshold = 0.0;
        material.alpha_policy = AlphaPolicy::Blend;
        material.base_color = [0.3, 0.4, 0.5];
        material.metallic = 0.8;
        material.roughness = 0.3;
        if textured || udim {
            let file = dir.join(format!("{i}.1001.png"));
            image::RgbaImage::from_pixel(
                2,
                2,
                image::Rgba([255, 255, 255, (alpha * 255.0).round() as u8]),
            )
            .save(&file)
            .unwrap();
            material.diffuse_texture = Some(
                if udim {
                    dir.join(format!("{i}.<UDIM>.png"))
                } else {
                    file
                }
                .to_string_lossy()
                .into_owned(),
            );
            material.uses_diffuse_texture_alpha = textured;
            if textured {
                material.opacity = 1.0;
            }
        }
        material.fingerprint = material.compute_fingerprint();
        snapshot
            .materials
            .insert(material.material_path.clone(), material.clone());
        if udim {
            snapshot
                .udim_tiles
                .insert(material.material_path.clone(), [1001].into());
        }
        let transform = Mat4::from_translation(Vec3::new(i as f32 - 2.5, 0.0, 0.0));
        let mut mesh = quad(matches!(family, Family::Skinned));
        if let Some(skin) = &mut Arc::make_mut(&mut mesh).skin {
            skin.joint_matrices[0] = transform.to_cols_array();
        }
        if matches!(family, Family::Instanced | Family::InstancedUdim) {
            snapshot.instanced_batches.push(RenderInstancedBatch {
                prototype_key: path.clone(),
                mesh,
                instance_transforms: vec![transform],
                instance_prim_paths: vec![path],
                material,
                purpose: RenderPurpose::Default,
                casts_shadow: false,
            });
        } else {
            snapshot.meshes.push(RenderMeshInstance {
                prim_path: path,
                mesh,
                world_matrix: transform,
                material,
                purpose: RenderPurpose::Default,
                casts_shadow: false,
            });
        }
    }
    snapshot.serial = next_mesh_serial();
    snapshot.instance_serial = snapshot.serial;
    snapshot
}

fn update(snapshot: &mut RenderWorldSnapshot, mut edit: impl FnMut(&mut RenderMaterialDesc)) {
    for material in snapshot.meshes.iter_mut().map(|m| &mut m.material).chain(
        snapshot
            .instanced_batches
            .iter_mut()
            .map(|b| &mut b.material),
    ) {
        edit(material);
        material.fingerprint = material.compute_fingerprint();
        snapshot
            .materials
            .insert(material.material_path.clone(), material.clone());
    }
    snapshot.serial = next_mesh_serial();
    snapshot.instance_serial = snapshot.serial;
}

fn read_centers(renderer: &Renderer, target: &FrameTarget) -> Vec<[f32; 4]> {
    // A single row of RGBA16F, already 256-byte aligned. No tonemapping or sRGB.
    let buffer = renderer.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("presence HDR readback"),
        size: u64::from(WIDTH * 8),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = renderer.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target.hdr_color_texture,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: 0,
                y: HEIGHT / 2,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(WIDTH * 8),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    renderer.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
    renderer
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = buffer.slice(..).get_mapped_range();
    (0..6)
        .map(|i| {
            std::array::from_fn(|c| {
                let offset = (i * 32 + 16) * 8 + c * 2;
                half::f16::from_le_bytes([bytes[offset], bytes[offset + 1]]).to_f32()
            })
        })
        .collect()
}

fn render(
    renderer: &mut Renderer,
    target: &mut FrameTarget,
    snapshot: &RenderWorldSnapshot,
    bg: f64,
) -> Vec<[f32; 4]> {
    target.clear_color = wgpu::Color {
        r: bg,
        g: bg,
        b: bg,
        a: 1.0,
    };
    renderer.try_render(target, snapshot, false).unwrap();
    read_centers(renderer, target)
}

fn assert_coverage(black: &[[f32; 4]], white: &[[f32; 4]], mask: bool, context: &str) {
    for (i, alpha) in ALPHAS.into_iter().enumerate() {
        let expected = if mask { f32::from(alpha >= 0.5) } else { alpha };
        for c in 0..3 {
            let recovered = 1.0 - (white[i][c] - black[i][c]);
            // Two half-float readbacks: bound cancellation/roundoff in bright HDR.
            let tolerance = 0.002 + 0.001 * (white[i][c].abs() + black[i][c].abs());
            assert!(
                (recovered - expected).abs() <= tolerance,
                "{context}: patch {i} channel {c}: coverage {recovered}, expected {expected}; black={:?} white={:?}",
                black[i],
                white[i]
            );
            if expected == 0.0 {
                assert_eq!(black[i][c], 0.0, "{context}: zero must add no radiance");
                assert_eq!(
                    white[i][c], 1.0,
                    "{context}: zero must not attenuate background"
                );
            }
        }
    }
}

#[test]
#[ignore = "requires headless GPU + OpenUSD link"]
fn presence_linear_hdr_coverage_all_shader_families() {
    let mut renderer = Renderer::new().expect("headless GPU");
    let mut target = renderer.create_frame_target(WIDTH, HEIGHT);
    for family in [
        Family::Static,
        Family::Instanced,
        Family::Skinned,
        Family::Udim,
        Family::InstancedUdim,
    ] {
        for textured in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut snapshot = patches(family, textured, dir.path());
            renderer.prefetch_textures(&snapshot);
            for (emission, exposure) in [(0.0, 1.0_f32), (1.0, 0.25), (1.0, 1.0), (4.0, 2.0)] {
                snapshot.camera.as_mut().unwrap().exposure_ev = if exposure == 1.0 {
                    0.0
                } else {
                    (1.0 / (1.2 * exposure)).log2()
                };
                update(&mut snapshot, |m| {
                    m.emissive = [0.15 * emission, 3.0 * emission, 1.95 * emission]
                });
                let black = render(&mut renderer, &mut target, &snapshot, 0.0);
                let white = render(&mut renderer, &mut target, &snapshot, 1.0);
                assert_coverage(
                    &black,
                    &white,
                    false,
                    &format!(
                        "{family:?}, texture={textured}, emission={emission}, exposure={exposure}"
                    ),
                );
                // Coverage alone would miss missing/double RGB premultiplication.
                // Compare each patch against its fully shaded, same-position limit.
                let mut full = snapshot.clone();
                update(&mut full, |m| {
                    m.opacity = 1.0;
                    m.uses_diffuse_texture_alpha = false;
                });
                let surface = render(&mut renderer, &mut target, &full, 0.0);
                for (i, alpha) in ALPHAS.into_iter().enumerate() {
                    for c in 0..3 {
                        let expected = surface[i][c] * alpha;
                        assert!(
                            (black[i][c] - expected).abs() < 0.002 + expected.abs() * 0.002,
                            "{family:?}: RGB must be premultiplied exactly once: patch {i}, channel {c}"
                        );
                    }
                }
            }
            // Mask rejects sub-threshold samples, fully covers surviving samples,
            // and takes precedence over either mode, even with emissive lighting.
            for mode in [OpacityMode::Presence, OpacityMode::Transparent] {
                update(&mut snapshot, |m| {
                    m.opacity_mode = mode;
                    m.alpha_policy = AlphaPolicy::Mask;
                    m.opacity_threshold = 0.5;
                });
                let black = render(&mut renderer, &mut target, &snapshot, 0.0);
                let white = render(&mut renderer, &mut target, &snapshot, 1.0);
                assert_coverage(
                    &black,
                    &white,
                    true,
                    &format!("{family:?} {mode:?} mask texture={textured}"),
                );
            }
        }
    }
}

#[test]
#[ignore = "requires headless GPU + OpenUSD link"]
fn presence_mode_only_edit_refreshes_warm_gpu_materials() {
    let mut renderer = Renderer::new().expect("headless GPU");
    let mut target = renderer.create_frame_target(WIDTH, HEIGHT);
    for family in [
        Family::Static,
        Family::Instanced,
        Family::Skinned,
        Family::Udim,
        Family::InstancedUdim,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut snapshot = patches(family, false, dir.path());
        renderer.prefetch_textures(&snapshot);
        update(&mut snapshot, |m| m.emissive = [0.15, 3.0, 1.95]);
        let presence = render(&mut renderer, &mut target, &snapshot, 0.0);
        update(&mut snapshot, |m| m.opacity_mode = OpacityMode::Transparent);
        let transparent = render(&mut renderer, &mut target, &snapshot, 0.0);
        assert_eq!(presence[0][1], 0.0, "{family:?} zero presence");
        assert!(
            transparent[0][1] >= 3.0,
            "{family:?} transparent emission survives"
        );
        update(&mut snapshot, |m| m.opacity_mode = OpacityMode::Presence);
        assert_eq!(
            render(&mut renderer, &mut target, &snapshot, 0.0),
            presence,
            "{family:?} undo"
        );
        update(&mut snapshot, |m| m.opacity_mode = OpacityMode::Transparent);
        assert_eq!(
            render(&mut renderer, &mut target, &snapshot, 0.0),
            transparent,
            "{family:?} redo"
        );
        update(&mut snapshot, |m| m.emissive = [0.0; 3]);
        assert!(
            render(&mut renderer, &mut target, &snapshot, 0.0)[0][0] > 0.001,
            "{family:?}: transparent reflection survives at opacity zero"
        );
    }
}

#[test]
#[ignore = "requires headless GPU + OpenUSD link"]
fn materialx_colored_glass_retains_transmission_without_presence() {
    let mut renderer = Renderer::new().expect("headless GPU");
    let mut target = renderer.create_frame_target(WIDTH, HEIGHT);
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = patches(Family::Static, false, dir.path());
    update(&mut snapshot, |m| {
        m.model = SurfaceModel::MaterialXStandardSurface;
        m.opacity_mode = OpacityMode::Transparent;
        m.transmission_color = [0.1, 0.8, 0.2];
        m.emissive = [0.15, 3.0, 1.95];
        assert!(m.has_colored_transmission());
    });
    let black = render(&mut renderer, &mut target, &snapshot, 0.0);
    let white = render(&mut renderer, &mut target, &snapshot, 1.0);
    for (i, alpha) in ALPHAS.into_iter().enumerate() {
        for (c, tint) in [0.1, 0.8, 0.2].into_iter().enumerate() {
            assert!(((white[i][c] - black[i][c]) - tint * (1.0 - alpha)).abs() < 0.005);
        }
    }
    update(&mut snapshot, |m| {
        m.model = SurfaceModel::UsdPreviewSurface;
        m.opacity_mode = OpacityMode::Presence;
        assert!(!m.has_colored_transmission());
    });
    let black = render(&mut renderer, &mut target, &snapshot, 0.0);
    let white = render(&mut renderer, &mut target, &snapshot, 1.0);
    assert_coverage(&black, &white, false, "presence ignores glass tint");
}
