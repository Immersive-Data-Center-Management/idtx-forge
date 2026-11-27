//! Screen-space HUD overlay rendering.
//!
//! Draws behavior-authored HUD elements — initially a center crosshair — on top
//! of the fully-rendered, tonemapped LDR frame. The HUD is driven per-frame off
//! the transient [`RenderWorldSnapshot::hud_elements`] channel the session fills
//! from ECS [`idtx_scene::HudComp`] data (which in turn is hydrated from the
//! `IdtxHud` USD schema a behavior authors into the session layer).
//!
//! Each element is a textured, alpha-blended quad positioned in NDC from an
//! anchor + pixel size (image native size when size is `[0, 0]`). The pass is
//! camera-independent: a center crosshair needs only the target dimensions, so
//! the renderer never consults the camera `view_proj`. Modelled on
//! [`crate::highlight::HighlightRenderer`]'s post-tonemap composite, but it
//! loads its image through the shared [`TextureCache`] (runtime image size).

use wgpu::util::DeviceExt;

use crate::scene_extract::RenderWorldSnapshot;
use crate::texture_cache::{TextureCache, TextureKind};
use crate::COLOR_FORMAT;

use idtx_scene::HudAnchor;

/// Per-element uniform: `rect = (center.xy, half.xy)` in NDC, `params.x` = opacity.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HudUniform {
    rect: [f32; 4],
    params: [f32; 4],
}

/// Renders screen-space HUD overlay quads over the LDR colour target.
pub struct HudRenderer {
    sampler: wgpu::Sampler,
    bgl: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

impl std::fmt::Debug for HudRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HudRenderer").finish_non_exhaustive()
    }
}

impl HudRenderer {
    /// Build the HUD composite pipeline. GPU resources per element are allocated
    /// lazily in [`Self::encode`] (there are typically only a handful of HUD
    /// elements, usually one crosshair).
    #[must_use]
    pub fn new(device: &wgpu::Device) -> Self {
        // Linear filtering so a native-size crosshair scales cleanly to an
        // explicit `sizePx`. Clamp to edge (the quad UVs stay in `[0, 1]`).
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("HUD Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("HUD BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("HUD Shader"),
            source: wgpu::ShaderSource::Wgsl(HUD_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("HUD Layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("HUD Pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            sampler,
            bgl,
            pipeline,
        }
    }

    /// Encode the HUD composite over `color_view` for this frame. No-op when the
    /// snapshot carries no HUD elements or the target is degenerate.
    ///
    /// `texture_cache` is borrowed mutably to load each element's image at its
    /// runtime size (a cache hit after the first sighting).
    pub fn encode(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture_cache: &mut TextureCache,
        snapshot: &RenderWorldSnapshot,
        color_view: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) {
        if snapshot.hud_elements.is_empty() || width == 0 || height == 0 {
            return;
        }

        // Build per-element GPU resources. Keep the texture Arcs alive for the
        // pass by holding them alongside the bind groups.
        let mut draws: Vec<(wgpu::BindGroup, std::sync::Arc<crate::texture_cache::GpuTexture>)> =
            Vec::with_capacity(snapshot.hud_elements.len());

        for el in &snapshot.hud_elements {
            if el.image_path.is_empty() || el.opacity <= 0.0 {
                continue;
            }
            let tex = texture_cache.get_or_load(
                &el.image_path,
                TextureKind::DiffuseSrgb,
                device,
                queue,
            );

            // Resolve on-screen pixel size: authored `sizePx` or native image size.
            let px_w = if el.size_px[0] > 0 {
                el.size_px[0] as f32
            } else {
                tex.width as f32
            };
            let px_h = if el.size_px[1] > 0 {
                el.size_px[1] as f32
            } else {
                tex.height as f32
            };

            let (center, half) =
                anchored_rect_ndc(el.anchor, px_w, px_h, width as f32, height as f32);

            let uniform = HudUniform {
                rect: [center[0], center[1], half[0], half[1]],
                params: [el.opacity.clamp(0.0, 1.0), 0.0, 0.0, 0.0],
            };
            let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("HUD Uniform"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("HUD BG"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&tex.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: ubuf.as_entire_binding(),
                    },
                ],
            });
            draws.push((bg, tex));
        }

        if draws.is_empty() {
            return;
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("HUD Encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("HUD Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            for (bg, _tex) in &draws {
                pass.set_bind_group(0, bg, &[]);
                pass.draw(0..6, 0..1);
            }
        }
        queue.submit([encoder.finish()]);
    }
}

/// Compute the `(center_ndc, half_ndc)` of a HUD element's on-screen rectangle
/// from its anchor and pixel size relative to the target. NDC x grows right in
/// `[-1, 1]`, y grows up in `[-1, 1]`. Edge/corner anchors inset the element by
/// its half-extent so it stays fully on-screen.
fn anchored_rect_ndc(
    anchor: HudAnchor,
    px_w: f32,
    px_h: f32,
    target_w: f32,
    target_h: f32,
) -> ([f32; 2], [f32; 2]) {
    // Half-extent in NDC: pixel size spans `2 * (px / target)` of the full
    // `[-1, 1]` range, so the half is `px / target`.
    let hx = px_w / target_w;
    let hy = px_h / target_h;
    let (cx, cy) = match anchor {
        HudAnchor::Center => (0.0, 0.0),
        HudAnchor::Top => (0.0, 1.0 - hy),
        HudAnchor::Bottom => (0.0, -1.0 + hy),
        HudAnchor::Left => (-1.0 + hx, 0.0),
        HudAnchor::Right => (1.0 - hx, 0.0),
        HudAnchor::TopLeft => (-1.0 + hx, 1.0 - hy),
        HudAnchor::TopRight => (1.0 - hx, 1.0 - hy),
        HudAnchor::BottomLeft => (-1.0 + hx, -1.0 + hy),
        HudAnchor::BottomRight => (1.0 - hx, -1.0 + hy),
    };
    ([cx, cy], [hx, hy])
}

/// Positions a quad in NDC from the uniform rect and samples the HUD image,
/// scaling its alpha by the element opacity. The target is sRGB, so the sampled
/// linear colour is encoded on write exactly like the beauty / highlight path.
const HUD_WGSL: &str = r"
struct HudUniform {
    rect: vec4<f32>,   // center.xy, half.xy (NDC)
    params: vec4<f32>, // opacity, _, _, _
};
@group(0) @binding(0) var samp: sampler;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var<uniform> u: HudUniform;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Two triangles covering the element rect: (BL, BR, TL) + (TL, BR, TR).
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
    );
    let c = corners[vi];
    var out: VsOut;
    out.pos = vec4<f32>(u.rect.xy + c * u.rect.zw, 0.0, 1.0);
    // Map corner [-1,1] → [0,1]; flip V so image top maps to screen top.
    out.uv = vec2<f32>((c.x + 1.0) * 0.5, (1.0 - c.y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let s = textureSample(tex, samp, in.uv);
    return vec4<f32>(s.rgb, s.a * u.params.x);
}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_anchor_is_origin() {
        let (c, h) = anchored_rect_ndc(HudAnchor::Center, 64.0, 64.0, 1920.0, 1080.0);
        assert_eq!(c, [0.0, 0.0]);
        assert!((h[0] - 64.0 / 1920.0).abs() < 1e-6);
        assert!((h[1] - 64.0 / 1080.0).abs() < 1e-6);
    }

    #[test]
    fn edge_anchors_inset_by_half_extent() {
        // A top-left element sits flush against the top-left corner, offset in by
        // its half-extent so no part clips off-screen.
        let (c, h) = anchored_rect_ndc(HudAnchor::TopLeft, 100.0, 50.0, 1000.0, 500.0);
        assert!((c[0] - (-1.0 + h[0])).abs() < 1e-6);
        assert!((c[1] - (1.0 - h[1])).abs() < 1e-6);
        // Left edge of the quad touches NDC x = -1.
        assert!((c[0] - h[0] - (-1.0)).abs() < 1e-6);
        // Top edge of the quad touches NDC y = +1.
        assert!((c[1] + h[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn bottom_right_anchor_touches_corner() {
        let (c, h) = anchored_rect_ndc(HudAnchor::BottomRight, 80.0, 80.0, 800.0, 800.0);
        // Right edge at +1, bottom edge at -1.
        assert!((c[0] + h[0] - 1.0).abs() < 1e-6);
        assert!((c[1] - h[1] - (-1.0)).abs() < 1e-6);
    }
}
