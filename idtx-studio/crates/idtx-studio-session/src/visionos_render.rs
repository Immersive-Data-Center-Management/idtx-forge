//! visionOS per-eye rendering into `CompositorServices` drawables (the
//! GPU half).
//!
//! The Metal mirror of [`xr_render`](crate::xr_render): the Swift
//! `CompositorServices` shell hands the standalone runtime one `cp_drawable`
//! `MTLTexture` per eye each frame (plus that eye's pose + frustum tangents,
//! sourced from ARKit + the drawable views). We:
//!
//! 1. render the scene once per eye into an offscreen [`FrameTarget`] with an
//!    off-axis projection built from the eye's tangents (identical camera math
//!    to the `OpenXR` path), then
//! 2. **blit** that resolved color into the imported drawable texture with a
//!    fullscreen pass built for the drawable's pixel format — a blit, not a
//!    `copy_texture_to_texture`, so the drawable can be any `CompositorServices`
//!    format (`rgba16Float`, `bgra8Unorm_srgb`, …) without a copy-compat
//!    constraint on the renderer's `Rgba8UnormSrgb` eye target.
//!
//! Importing the drawable texture mirrors [`xr_render`]'s `wrap_one_vk` /
//! `wrap_one`, via `wgpu::hal::metal::Device::texture_from_raw`. The drawable is
//! owned by `CompositorServices`; we `retain` it so wgpu's `Texture` drop
//! `release`s the extra ref (net-zero), never freeing the compositor's image.
//!
//! Compiled only for `feature = "visionos"` on an Apple target. Needs a Metal
//! device + the Simulator/headset, so it is not unit-tested here (the camera
//! math is covered by the `OpenXR` path's `depth_range_matches_projection_convention`).

// Docs are dense with Metal / CompositorServices / wgpu identifiers that read
// fine unbackticked.
#![allow(clippy::doc_markdown)]

use std::ffi::c_void;

use glam::{Mat4, Vec3, Vec4};
use idtx_platform_xr::view::XrView;
use idtx_render::pipeline::{BlitPipeline, build_blit_pipeline};
use idtx_render::{FrameTarget, RenderCameraDesc, RenderWorldSnapshot, Renderer};

use crate::StudioSession;

/// Near/far clip planes for the immersive projection (metres). Matches the
/// `OpenXR` path's `render_xr` constants.
const NEAR: f32 = 0.05;
const FAR: f32 = 5000.0;

/// Per-eye offscreen render scale: the beauty + tonemap + shadow passes (the
/// pixel-bound GPU cost) render into eye targets at `RENDER_SCALE × drawable`
/// resolution, then the fullscreen blit upscales the color into the native
/// drawable (its sampler already filters) and a depth-upscale pass fills the
/// native drawable depth for `CompositorServices` reprojection. `0.6` ≈ 0.36 area
/// ≈ ~64% off the pixel-bound passes (the on-device `pbr`/`tonemap` GPU lines).
///
/// **`1.0` disables scaling and restores the exact proven present path**
/// (resolve → `copy_texture_to_texture`, no upscale pass) — the one-line revert
/// if the scaled depth path ever misbehaves on device (a wrong drawable depth
/// shows as black immersive content). Device-validate any value < 1.0.
pub const RENDER_SCALE: f32 = 0.8;

/// Scaled per-eye render dimensions for a given native drawable size, clamped to
/// at least 1px. At [`RENDER_SCALE`] == 1.0 this is the identity.
fn scaled_render_dims(width: u32, height: u32) -> (u32, u32) {
    let rw = ((width as f32 * RENDER_SCALE).round() as u32).max(1);
    let rh = ((height as f32 * RENDER_SCALE).round() as u32).max(1);
    (rw, rh)
}

/// Per-frame render metrics captured by [`VisionosGpu::render`] when built with
/// `--features metrics`. The visionOS analogue of the `OpenXR` path's `EyeTimings`
/// (in the `idtx_xr`-gated `xr_render` module, which is not compiled here).
///
/// Holds each eye's full [`RenderFrameTimings`](idtx_render::metrics::RenderFrameTimings)
/// (the renderer overwrites its single `last_frame_timings` per eye, so it must
/// be cloned inside the loop) plus the CPU cost of the visionOS-only present step
/// (offscreen→drawable blit + MSAA depth resolve + drawable copy + submit), which
/// lives outside the renderer's own instrumentation.
#[cfg(feature = "metrics")]
#[derive(Debug, Clone, Default)]
pub(crate) struct VisionosEyeMetrics {
    /// Per-eye renderer timings (index 0 = left, 1 = right); unused slots stay
    /// `Default` for a mono/one-view frame.
    pub frames: [idtx_render::metrics::RenderFrameTimings; 2],
    /// Per-eye CPU wall-clock of the present section (blit + depth resolve/copy +
    /// `queue.submit`). Maps to `XrFrameTimings::eye_swapchain_copy_ns`.
    pub present_ns: [u64; 2],
}

/// Return type of [`VisionosGpu::render`]: the captured per-eye metrics with the
/// `metrics` feature, `()` otherwise (zero-overhead, no-op).
#[cfg(feature = "metrics")]
type RenderCapture = VisionosEyeMetrics;
#[cfg(not(feature = "metrics"))]
type RenderCapture = ();

use crate::presentation::SnapshotMaint;

/// Assemble one [`xr_metrics::XrFrameTimings`](crate::xr_metrics::XrFrameTimings)
/// from the captured per-eye [`VisionosEyeMetrics`] plus the snapshot-extract
/// timing for this frame. Pure (no GPU / no session state) so it is unit-tested
/// without a headset. The `OpenXR`-only pacing fields (`wait_frame_ns`, …) stay 0 —
/// `CompositorServices` has no equivalent counters.
#[cfg(feature = "metrics")]
pub(crate) fn build_visionos_frame_timings(
    eye: &VisionosEyeMetrics,
    total_ns: u64,
    extract_ns: u64,
    maint: &SnapshotMaint,
) -> crate::xr_metrics::XrFrameTimings {
    let f = &eye.frames;
    crate::xr_metrics::XrFrameTimings {
        total_ns,
        extract_ns,
        extracted_this_frame: maint.extracted,
        full_extract_ns: maint.full_extract_ns,
        patch_transform_ns: maint.patch_transform_ns,
        patch_skeleton_ns: maint.patch_skeleton_ns,
        patch_widget_ns: maint.patch_widget_ns,
        eye_render_ns: [f[0].total_ns, f[1].total_ns],
        eye_mesh_count: [f[0].mesh_count, f[1].mesh_count],
        eye_instance_count: [f[0].instance_count, f[1].instance_count],
        eye_cache_misses: [f[0].cache_misses, f[1].cache_misses],
        eye_evictions: [f[0].evictions, f[1].evictions],
        eye_draw_list_ns: [f[0].draw_list_build_ns, f[1].draw_list_build_ns],
        eye_gpu_passes_ns: [f[0].gpu_passes_ns, f[1].gpu_passes_ns],
        eye_gpu_pbr_ns: [f[0].gpu_pbr_ns, f[1].gpu_pbr_ns],
        eye_gpu_sample_id: [f[0].gpu_sample_id, f[1].gpu_sample_id],
        eye_viewport: [f[0].viewport, f[1].viewport],
        eye_gpu_tonemap_ns: [f[0].gpu_tonemap_ns, f[1].gpu_tonemap_ns],
        eye_gpu_shadow_ns: [f[0].gpu_shadow_ns, f[1].gpu_shadow_ns],
        eye_overdraw: [
            f[0].fragment_shader_invocations,
            f[1].fragment_shader_invocations,
        ],
        eye_gpu_wait_ns: [f[0].gpu_wait_ns, f[1].gpu_wait_ns],
        // The visionOS present cost (blit + depth resolve/copy + submit) is the
        // nearest analogue to the OpenXR swapchain-copy; depth-copy is folded in.
        eye_swapchain_copy_ns: eye.present_ns,
        eye_draw_calls: [f[0].draw_calls, f[1].draw_calls],
        eye_pipeline_switches: [f[0].pipeline_switches, f[1].pipeline_switches],
        eye_bind_group_sets: [f[0].bind_group_sets, f[1].bind_group_sets],
        eye_merged_batch_count: [f[0].merged_batch_count, f[1].merged_batch_count],
        ..Default::default()
    }
}

/// One eye's per-frame input from the Swift `CompositorServices` shell: the
/// drawable color `MTLTexture` to present into, plus that eye's located pose +
/// frustum tangents (already converted to the neutral [`XrView`] by the
/// [`CompositorServicesBackend`](idtx_platform_visionos::CompositorServicesBackend)).
#[derive(Debug, Clone, Copy)]
pub struct VisionosEyeFrame {
    /// The `CompositorServices` `cp_drawable` color texture (`id<MTLTexture>`),
    /// valid for this frame only.
    pub color_texture: *mut c_void,
    /// The `cp_drawable` depth texture (`id<MTLTexture>`, Depth32Float) for this
    /// eye, or null if the layer has no depth. The device compositor needs
    /// per-pixel depth for reprojection, so the eye's resolved depth is copied in.
    pub depth_texture: *mut c_void,
    /// This eye's pose + projection FOV for the frame.
    pub view: XrView,
}

/// Per-eye GPU resources for immersive rendering: one offscreen render target
/// per view, the fullscreen blit pipeline built for the drawable's format, and
/// one cached bind group per eye (binding that eye's **stable** offscreen color
/// view — only the drawable destination changes per frame).
pub struct VisionosGpu {
    eye_targets: Vec<FrameTarget>,
    /// Per-eye single-sample depth resolve targets. At scale 1.0 these carry
    /// `RENDER_ATTACHMENT | COPY_SRC` and are copied 1:1 into the drawable depth;
    /// at scale < 1.0 they carry `RENDER_ATTACHMENT | TEXTURE_BINDING` and are
    /// upscaled into the native drawable depth by [`DepthUpscale`].
    depth_resolve: Vec<(wgpu::Texture, wgpu::TextureView)>,
    blit: BlitPipeline,
    bind_groups: Vec<wgpu::BindGroup>,
    /// Native drawable width/height (the present/drawable size).
    width: u32,
    height: u32,
    /// Scaled per-eye render width/height (`RENDER_SCALE × drawable`). Equal to
    /// `width`/`height` when `RENDER_SCALE == 1.0`.
    render_width: u32,
    render_height: u32,
    /// Depth-upscale pass + per-eye bind groups, present only when rendering
    /// below native (`RENDER_SCALE < 1.0`). `None` → the proven resolve+copy path.
    depth_upscale: Option<DepthUpscale>,
    drawable_format: wgpu::TextureFormat,
}

/// Fullscreen nearest-neighbour upscale of a scaled single-sample depth texture
/// into the native drawable depth attachment (`Depth32Float`). Only built when
/// [`RENDER_SCALE`] < 1.0. Writes `@builtin(frag_depth)` so the compositor's
/// reprojection still gets per-pixel depth at native resolution.
struct DepthUpscale {
    pipeline: wgpu::RenderPipeline,
    /// Per-eye `{ scale: vec2<f32>, pad: vec2<f32> }` uniform (render/native ratio).
    /// Held to keep the buffers alive for the lifetime of the bind groups.
    #[allow(dead_code)]
    params: Vec<wgpu::Buffer>,
    /// Per-eye bind group: the scaled depth + that eye's params buffer.
    bind_groups: Vec<wgpu::BindGroup>,
}

/// WGSL for [`DepthUpscale`]: fullscreen triangle, samples the scaled depth by
/// `floor(frag_coord × scale)` (nearest), outputs `frag_depth`.
const DEPTH_UPSCALE_WGSL: &str = r#"
struct Params { scale: vec2<f32>, pad: vec2<f32> };
@group(0) @binding(0) var src_depth: texture_depth_2d;
@group(0) @binding(1) var<uniform> params: Params;

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(-1.0,  3.0),
        vec2<f32>( 3.0, -1.0),
    );
    return vec4<f32>(p[idx], 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @builtin(frag_depth) f32 {
    let dims = vec2<i32>(textureDimensions(src_depth));
    let src = vec2<i32>(floor(pos.xy * params.scale));
    let c = clamp(src, vec2<i32>(0, 0), dims - vec2<i32>(1, 1));
    return textureLoad(src_depth, c, 0);
}
"#;

impl DepthUpscale {
    /// Build the depth-upscale pipeline for the drawable depth format
    /// (`Depth32Float`) and the per-eye bind groups over `depth_resolve`.
    fn new(
        device: &wgpu::Device,
        depth_resolve: &[(wgpu::Texture, wgpu::TextureView)],
        render_width: u32,
        render_height: u32,
        width: u32,
        height: u32,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("visionos-depth-upscale-shader"),
            source: wgpu::ShaderSource::Wgsl(DEPTH_UPSCALE_WGSL.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("visionos-depth-upscale-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("visionos-depth-upscale-layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("visionos-depth-upscale-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        // scale = render/native (dst→src ratio for the nearest fetch).
        let sx = render_width as f32 / width as f32;
        let sy = render_height as f32 / height as f32;
        // { scale: vec2<f32>, pad: vec2<f32> } — 16 bytes, native-endian.
        let mut uniform_bytes = [0u8; 16];
        uniform_bytes[0..4].copy_from_slice(&sx.to_ne_bytes());
        uniform_bytes[4..8].copy_from_slice(&sy.to_ne_bytes());
        let mut params = Vec::with_capacity(depth_resolve.len());
        let mut bind_groups = Vec::with_capacity(depth_resolve.len());
        for (_, view) in depth_resolve {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("visionos-depth-upscale-params"),
                size: uniform_bytes.len() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: true,
            });
            buf.slice(..)
                .get_mapped_range_mut()
                .copy_from_slice(&uniform_bytes);
            buf.unmap();
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("visionos-depth-upscale-bg"),
                layout: &bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: buf.as_entire_binding(),
                    },
                ],
            });
            params.push(buf);
            bind_groups.push(bg);
        }
        Self {
            pipeline,
            params,
            bind_groups,
        }
    }
}

impl std::fmt::Debug for VisionosGpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisionosGpu")
            .field("extent", &(self.width, self.height))
            .field("render_extent", &(self.render_width, self.render_height))
            .field("views", &self.eye_targets.len())
            .field("drawable_format", &self.drawable_format)
            .finish_non_exhaustive()
    }
}

impl VisionosGpu {
    /// Allocate per-eye offscreen targets + the blit pipeline for `view_count`
    /// views at `width`×`height`, presenting into `drawable_format` textures.
    fn new(
        renderer: &Renderer,
        width: u32,
        height: u32,
        view_count: usize,
        drawable_format: wgpu::TextureFormat,
    ) -> Self {
        let blit = build_blit_pipeline(&renderer.device, drawable_format);
        // Render the scene below native (RENDER_SCALE); the blit upscales color and
        // the depth-upscale pass fills the native drawable depth. At scale 1.0 the
        // render dims equal the drawable and the proven resolve+copy path is kept.
        let (render_width, render_height) = scaled_render_dims(width, height);
        let scaled = render_width != width || render_height != height;
        // Depth-resolve-capable eye targets (MSAA depth is sampleable for the
        // resolve pass), mirroring the OpenXR depth-layer path. Allocated at the
        // scaled render resolution.
        let eye_targets: Vec<FrameTarget> = (0..view_count)
            .map(|_| renderer.create_xr_eye_target(render_width, render_height))
            .collect();
        // When scaled, the resolved depth is sampled by the upscale pass
        // (TEXTURE_BINDING); at 1.0 it is copied 1:1 into the drawable (COPY_SRC).
        let depth_resolve: Vec<(wgpu::Texture, wgpu::TextureView)> = (0..view_count)
            .map(|_| make_depth_resolve_target(&renderer.device, render_width, render_height, scaled))
            .collect();
        let depth_upscale = if scaled {
            Some(DepthUpscale::new(
                &renderer.device,
                &depth_resolve,
                render_width,
                render_height,
                width,
                height,
            ))
        } else {
            None
        };
        let bind_groups = eye_targets
            .iter()
            .map(|t| {
                renderer.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("visionos-eye-blit-bg"),
                    layout: &blit.bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Sampler(&blit.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&t.color_view),
                        },
                    ],
                })
            })
            .collect();
        Self {
            eye_targets,
            depth_resolve,
            blit,
            bind_groups,
            width,
            height,
            render_width,
            render_height,
            depth_upscale,
            drawable_format,
        }
    }

    /// Whether these resources already match the requested frame layout.
    fn matches(
        &self,
        width: u32,
        height: u32,
        view_count: usize,
        drawable_format: wgpu::TextureFormat,
    ) -> bool {
        self.width == width
            && self.height == height
            && self.eye_targets.len() == view_count
            && self.drawable_format == drawable_format
    }

    /// Render every eye into its offscreen target, then blit each into the
    /// matching imported drawable texture.
    fn render(
        &self,
        renderer: &mut Renderer,
        snapshot: &mut RenderWorldSnapshot,
        eyes: &[VisionosEyeFrame],
        anchor: Mat4,
        recenter: Vec3,
        extra_lines: &[idtx_render::OverlayLine],
    ) -> RenderCapture {
        #[cfg(feature = "metrics")]
        let mut cap = VisionosEyeMetrics::default();
        for (idx, eye) in eyes.iter().enumerate() {
            // 1. Render the scene for this eye into its offscreen target.
            snapshot.camera = Some(eye_camera(&eye.view, anchor, recenter));
            // Render *with* the behavior-driven overlay composites (tint +
            // silhouette outline), exactly like the OpenXR `render_eyes`. A plain
            // `render` here skips `highlight.encode`, so behavior highlights never
            // reached the headset even though the snapshot carried them.
            // `extra_lines` carries behavior-drawn overlay geometry (the teleport
            // pointer arc + landing marker); the line pass early-outs when empty.
            // No editor selection OBB in the immersive path (`None`).
            #[cfg(feature = "metrics")]
            renderer.set_metrics_view(idx);
            renderer.render_with_overlay(&self.eye_targets[idx], snapshot, None, extra_lines, false);

            // Capture this eye's renderer timings before the next eye overwrites
            // the renderer's single `last_frame_timings` slot.
            #[cfg(feature = "metrics")]
            if idx < cap.frames.len() {
                cap.frames[idx] = renderer.last_frame_timings().clone();
            }

            // Everything below (present) is the visionOS-only cost the renderer
            // does not see; time it per eye.
            #[cfg(feature = "metrics")]
            let t_present = std::time::Instant::now();

            // 2. Import the compositor's drawable texture and blit the eye color
            //    into it (format-converting fullscreen pass).
            // SAFETY: `color_texture` is a live `id<MTLTexture>` from the current
            // `cp_drawable`, valid for this frame; `import_drawable_color` retains
            // it so the `wgpu::Texture` drop below only balances our own retain.
            let drawable =
                unsafe { import_drawable_color(&renderer.device, eye, self.width, self.height, self.drawable_format) };
            let dst = drawable.create_view(&wgpu::TextureViewDescriptor::default());

            let mut encoder =
                renderer
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("visionos-eye-present"),
                    });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("visionos-eye-blit"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &dst,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.blit.pipeline);
                pass.set_bind_group(0, &self.bind_groups[idx], &[]);
                pass.draw(0..3, 0..1);
            }

            // 3. Depth for the compositor's reprojection: resolve this eye's MSAA
            //    depth to single-sample **reverse-Z** (1=near, 0=far) at the scaled
            //    render resolution, then get it into the native drawable depth.
            //    CompositorServices only supports reverse-Z drawable depth; the
            //    renderer draws forward-Z, so the resolve flips it. The Swift shell
            //    declares the matching near/far via `drawable.depthRange =
            //    simd_float2(FAR, NEAR)`. The device compositor drops color-only
            //    content (shows black) without valid per-pixel depth; Simulator is
            //    lenient.
            renderer.resolve_depth_reverse_z(
                &mut encoder,
                &self.eye_targets[idx].depth_view,
                &self.depth_resolve[idx].1,
            );
            // SAFETY: `depth_texture` is a live `id<MTLTexture>` (Depth32Float) from
            // the current `cp_drawable` when non-null; imported RENDER_ATTACHMENT |
            // COPY_DST, retained so the drop below balances our retain.
            let depth_drawable = if eye.depth_texture.is_null() {
                None
            } else {
                Some(unsafe {
                    import_drawable_depth(&renderer.device, eye.depth_texture, self.width, self.height)
                })
            };
            if let Some(dd) = &depth_drawable {
                match &self.depth_upscale {
                    // Scaled render: nearest-upscale the scaled resolved depth into
                    // the native drawable depth (a size-changing copy is illegal).
                    Some(up) => {
                        let dd_view = dd.create_view(&wgpu::TextureViewDescriptor::default());
                        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("visionos-depth-upscale"),
                            color_attachments: &[],
                            depth_stencil_attachment: Some(
                                wgpu::RenderPassDepthStencilAttachment {
                                    view: &dd_view,
                                    depth_ops: Some(wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(0.0),
                                        store: wgpu::StoreOp::Store,
                                    }),
                                    stencil_ops: None,
                                },
                            ),
                            timestamp_writes: None,
                            occlusion_query_set: None,
                            multiview_mask: None,
                        });
                        pass.set_pipeline(&up.pipeline);
                        pass.set_bind_group(0, &up.bind_groups[idx], &[]);
                        pass.draw(0..3, 0..1);
                    }
                    // Native render (RENDER_SCALE == 1.0): the proven 1:1 copy.
                    None => {
                        encoder.copy_texture_to_texture(
                            self.depth_resolve[idx].0.as_image_copy(),
                            dd.as_image_copy(),
                            wgpu::Extent3d {
                                width: self.width,
                                height: self.height,
                                depth_or_array_layers: 1,
                            },
                        );
                    }
                }
            }
            renderer.queue.submit([encoder.finish()]);
            #[cfg(feature = "metrics")]
            if idx < cap.present_ns.len() {
                cap.present_ns[idx] = t_present.elapsed().as_nanos() as u64;
            }
            // `drawable` (+ `depth_drawable`) drop here: wgpu `release`s the
            // textures, balancing the `retain`s so CompositorServices keeps them.
        }
        #[cfg(feature = "metrics")]
        return cap;
    }
}

impl StudioSession {
    /// Render one immersive stereo frame into the `CompositorServices` drawables.
    ///
    /// `eyes` is one [`VisionosEyeFrame`] per view (two for Vision Pro). The
    /// drawables must be `width`×`height` with pixel format `drawable_format`
    /// (the wgpu equivalent of the `cp_layer_renderer`'s color format). Ensures
    /// the scene snapshot and the per-eye GPU resources, then renders + blits
    /// every eye. The caller (the standalone runtime) submits the compositor
    /// present after this returns (wgpu work for the frame is already queued and
    /// ordered on the shared Metal device).
    /// Allocate incoming per-eye targets and pipelines on the loading worker,
    /// using the live compositor extent/format captured by the transition driver.
    pub fn prepare_visionos_resources(&mut self, width: u32, height: u32, views: usize, format: wgpu::TextureFormat) {
        self.ensure_visionos_snapshot();
        if self.visionos_gpu.as_ref().is_none_or(|gpu| !gpu.matches(width, height, views, format)) {
            self.visionos_gpu = Some(VisionosGpu::new(&self.renderer, width, height, views, format));
        }
    }

    pub fn visionos_render_frame(
        &mut self,
        eyes: &[VisionosEyeFrame],
        width: u32,
        height: u32,
        drawable_format: wgpu::TextureFormat,
    ) {
        if eyes.is_empty() {
            return;
        }
        #[cfg(feature = "metrics")]
        let t_total = std::time::Instant::now();
        #[cfg(feature = "metrics")]
        let t_extract = std::time::Instant::now();
        let maint = self.ensure_visionos_snapshot();
        #[cfg(feature = "metrics")]
        let extract_ns = t_extract.elapsed().as_nanos() as u64;
        #[cfg(not(feature = "metrics"))]
        let _ = maint;

        // Viewer placement, two sources:
        //
        // 1. **Local-player camera** (preferred): when the runtime bound the viewer
        //    to the injected player rig via `use_scene_camera`, the anchor is
        //    `(basis * camera_world).inverse()` — the ARKit head pose composed with
        //    the camera prim's live world transform ("player rig + headset offset").
        //    Recomputed every frame (no lock), so a locomotion behavior moving
        //    `/World/Player` moves the viewer and the viewer spawns at the camera's
        //    authored world transform (the origin for an identity rig).
        // 2. **AABB fallback** (no player camera): drop the scene bounding-box centre
        //    a comfortable distance ahead. During warmup (ARKit tracking settling —
        //    the first frames can report an identity head pose) the model is
        //    head-locked so it's always visible; once settled we LOCK a room anchor
        //    from the current pose so the user can walk around it.
        const ANCHOR_WARMUP_FRAMES: u32 = 45;
        // Sentinel in `visionos_anchor_warmup` marking the one-shot player-anchor
        // diagnostic as already emitted (the AABB warmup counter never reaches it).
        const PLAYER_ANCHOR_LOGGED: u32 = u32::MAX;
        let anchor = if let Some(a) = self.visionos_scene_camera_anchor() {
            if self.visionos_anchor_warmup != PLAYER_ANCHOR_LOGGED {
                visionos_diag(&format!(
                    "anchor source: local-player camera; basis_det={:.4} translation={:?}",
                    self.projection_basis.determinant(),
                    a.w_axis.truncate(),
                ));
                self.visionos_anchor_warmup = PLAYER_ANCHOR_LOGGED;
            }
            a
        } else {
            let usd_aabb = crate::compute_scene_aabb_from_ecs(&self.hydrator);
            if let Some(a) = self.visionos_anchor {
                a
            } else {
                let fresh = compute_visionos_anchor(usd_aabb, self.projection_basis, &eyes[0].view);
                if self.visionos_anchor_warmup >= ANCHOR_WARMUP_FRAMES {
                    let (meshes, lights) = self
                        .last_snapshot
                        .as_ref()
                        .map_or((usize::MAX, usize::MAX), |s| (s.meshes.len(), s.lights.len()));
                    visionos_diag(&format!(
                        "anchor locked: usd_aabb={usd_aabb:?} basis_det={:.4} target={:?} meshes={meshes} lights={lights}",
                        self.projection_basis.determinant(),
                        fresh.w_axis.truncate(),
                    ));
                    self.visionos_anchor = Some(fresh);
                } else {
                    self.visionos_anchor_warmup += 1;
                }
                fresh
            }
        };

        let need_new = self
            .visionos_gpu
            .as_ref()
            .is_none_or(|g| !g.matches(width, height, eyes.len(), drawable_format));
        if need_new {
            self.visionos_gpu = Some(VisionosGpu::new(
                &self.renderer,
                width,
                height,
                eyes.len(),
                drawable_format,
            ));
        }

        // Disjoint field borrows: renderer, last_snapshot, visionos_gpu.
        let Some(snapshot) = self.last_snapshot.as_mut() else {
            return;
        };
        let gpu = self
            .visionos_gpu
            .as_ref()
            .expect("visionos_gpu set just above");
        #[cfg(feature = "metrics")]
        let eye_metrics = gpu.render(
            &mut self.renderer,
            snapshot,
            eyes,
            anchor,
            self.xr_recenter,
            &self.presentation_overlay_lines,
        );
        #[cfg(not(feature = "metrics"))]
        gpu.render(
            &mut self.renderer,
            snapshot,
            eyes,
            anchor,
            self.xr_recenter,
            &self.presentation_overlay_lines,
        );

        // Record + periodically dump the per-eye render metrics. The `gpu` /
        // `snapshot` borrows end above (NLL), so `&mut self.xr_metrics` is free.
        #[cfg(feature = "metrics")]
        {
            let total_ns = t_total.elapsed().as_nanos() as u64;
            let timings = build_visionos_frame_timings(
                &eye_metrics,
                total_ns,
                extract_ns,
                &maint,
            );
            self.xr_metrics.record(timings);
            // Log + dump on the same cadence (`should_log`, every LOG_EVERY=120
            // frames). A slow scene renders far below 90 fps, so a 300-frame
            // (WINDOW) dump threshold would need ~tens of seconds — too coarse to
            // yield a file on a short device run. Dumping on `should_log` writes a
            // rolling JSON every ~120 frames; the teardown flush
            // (`visionos_flush_metrics`) still covers runs shorter than that.
            if self.xr_metrics.should_log() {
                tracing::info!("{}", self.xr_metrics.log_line());
                let stage = self.visionos_metrics_stage_name();
                self.xr_metrics.write_json(&stage);
            }
        }
    }

    /// Stage name used for the metrics JSON filename
    /// (`idtx-xr-metrics-<stage>-<ts>.json`), derived from the open stage path.
    #[cfg(feature = "metrics")]
    fn visionos_metrics_stage_name(&self) -> String {
        self.source_path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("stage")
            .to_owned()
    }

    /// Flush the rolling visionOS render metrics to a JSON dump in the app's
    /// writable temp dir (the devicectl-pullable `tmp/` container). Called by the
    /// runtime on immersive-session teardown so a short run still emits a file.
    /// Best-effort; no-op when no frames were recorded. See
    /// [`xr_metrics`](crate::xr_metrics) for the schema and the
    /// `pull-visionos-metrics` xtask for retrieval.
    #[cfg(feature = "metrics")]
    pub fn visionos_flush_metrics(&self) {
        let stage = self.visionos_metrics_stage_name();
        self.xr_metrics.write_json(&stage);
    }

    /// Force a full scene re-extract on the next
    /// [`visionos_render_frame`](Self::visionos_render_frame). The standalone
    /// runtime calls this after ticking Pulley behaviors against
    /// [`world_mut`](Self::world_mut), since those ECS mutations happen outside
    /// the session and would otherwise not mark the snapshot dirty.
    pub fn mark_snapshot_dirty(&mut self) {
        self.snapshot_dirty = true;
    }

    /// Mark the snapshot dirty **only** if a behavior moved an entity transform
    /// directly this frame, returning whether it did.
    ///
    /// Replaces the visionOS runtime's former *unconditional* per-frame
    /// [`mark_snapshot_dirty`](Self::mark_snapshot_dirty) — the bring-up shortcut
    /// that pinned `extracted_this_frame` at 100% and defeated the shared
    /// incremental-snapshot machinery (full `extract_with_shadow` every frame →
    /// per-frame `from_mesh_comp` re-run + GPU-cache re-key churn + merge re-bake).
    ///
    /// The `OpenXR`/desktop paths never force-extract; they rely on the
    /// domain-classified `tick()` router (run here by
    /// `apply_session_edit_commands` / `tick_physics` / `tick_exec`) to set
    /// `snapshot_dirty` or the cheap transform/skeleton/widget patch flags. The
    /// single change that router does *not* observe is a behavior writing an
    /// entity transform directly through the SDK `set_xform`
    /// ([`ecs::Transform`] + [`ecs::TransformDirty`]) — on the `OpenXR` path that is
    /// consumed by the per-controller sweep in `xr_preview`, which this
    /// standalone runtime does not drive. So force a re-extract on exactly those
    /// frames, and clear the markers (nothing else consumes them on this path) so
    /// the force does not persist. A datasource/session-edit-only frame (the
    /// common case for the datacenter telemetry scene) leaves the snapshot on the
    /// cheap patch path.
    pub fn mark_dirty_if_behaviors_moved_transforms(&mut self) -> bool {
        use idtx_behavior::ecs as bec;
        use idtx_scene::bevy_ecs::prelude::{Entity, With};

        let world = self.hydrator.world_mut();
        let mut dirty_q = world.query_filtered::<Entity, With<bec::TransformDirty>>();
        let moved: Vec<Entity> = dirty_q.iter(world).collect();
        if moved.is_empty() {
            return false;
        }
        for e in moved {
            world.entity_mut(e).remove::<bec::TransformDirty>();
        }
        self.snapshot_dirty = true;
        true
    }

    /// Prepare the shared scene/resource/overlay state before eye presentation.
    fn ensure_visionos_snapshot(&mut self) -> SnapshotMaint {
        self.prepare_presentation(1.0)
    }
}

/// Import a `CompositorServices` drawable color `MTLTexture` as a wgpu texture.
///
/// # Safety
///
/// `eye.color_texture` must be a live, non-null `id<MTLTexture>` from the
/// current `cp_drawable`, matching `width`/`height`/`format`. The texture is
/// `retain`ed, so the returned `wgpu::Texture`'s `release` on drop balances it
/// and the compositor keeps ownership.
unsafe fn import_drawable_color(
    device: &wgpu::Device,
    eye: &VisionosEyeFrame,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_metal::{MTLTexture, MTLTextureType};

    let obj = eye.color_texture.cast::<ProtocolObject<dyn MTLTexture>>();
    // SAFETY: caller guarantees a live non-null MTLTexture; retain balances the
    // release wgpu performs when the wrapping Texture drops.
    let retained = unsafe { Retained::retain(obj) }
        .expect("CompositorServices drawable color texture must be non-null");

    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    // SAFETY: `retained` is a valid single-sample 2D MTLTexture compatible with
    // `format`; the compositor created the drawable with that pixel format.
    let hal_tex = unsafe {
        wgpu::hal::metal::Device::texture_from_raw(
            retained,
            format,
            MTLTextureType::Type2D,
            1,
            1,
            wgpu::hal::CopyExtent {
                width,
                height,
                depth: 1,
            },
        )
    };
    // SAFETY: `hal_tex` was built from this device's Metal backend per `desc`.
    unsafe {
        device.create_texture_from_hal::<wgpu::hal::api::Metal>(
            hal_tex,
            &wgpu::TextureDescriptor {
                label: Some("visionos-drawable-color"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
        )
    }
}

/// A single-sample `Depth32Float` resolve target (`RENDER_ATTACHMENT |
/// COPY_SRC`): [`Renderer::resolve_depth`] writes the resolved eye depth here,
/// then it is copied into the drawable's depth texture. Mirrors the `OpenXR` path's
/// `make_depth_resolve_target`.
fn make_depth_resolve_target(
    device: &wgpu::Device,
    w: u32,
    h: u32,
    bindable: bool,
) -> (wgpu::Texture, wgpu::TextureView) {
    // `bindable` (scaled render): the resolved depth is sampled by the
    // depth-upscale pass → needs TEXTURE_BINDING. Unscaled: it is copied 1:1 into
    // the drawable depth → needs COPY_SRC.
    let usage = if bindable {
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING
    } else {
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC
    };
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("visionos-depth-resolve"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    (tex, view)
}

/// Import a `CompositorServices` drawable **depth** `MTLTexture` (`Depth32Float`) as
/// a wgpu texture with `COPY_DST`, the destination for the resolved eye depth copy.
///
/// # Safety
///
/// `raw` must be a live, non-null `id<MTLTexture>` (`Depth32Float`) from the
/// current `cp_drawable`, matching `width`/`height`. It is `retain`ed; the
/// returned texture's `release` on drop balances it.
unsafe fn import_drawable_depth(
    device: &wgpu::Device,
    raw: *mut c_void,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_metal::{MTLTexture, MTLTextureType};

    let obj = raw.cast::<ProtocolObject<dyn MTLTexture>>();
    // SAFETY: caller guarantees a live non-null MTLTexture.
    let retained = unsafe { Retained::retain(obj) }
        .expect("CompositorServices drawable depth texture must be non-null");
    let format = wgpu::TextureFormat::Depth32Float;
    // SAFETY: `retained` is a valid single-sample 2D Depth32Float MTLTexture.
    let hal_tex = unsafe {
        wgpu::hal::metal::Device::texture_from_raw(
            retained,
            format,
            MTLTextureType::Type2D,
            1,
            1,
            wgpu::hal::CopyExtent {
                width,
                height,
                depth: 1,
            },
        )
    };
    // SAFETY: `hal_tex` was built from this device's Metal backend per `desc`.
    unsafe {
        device.create_texture_from_hal::<wgpu::hal::api::Metal>(
            hal_tex,
            &wgpu::TextureDescriptor {
                label: Some("visionos-drawable-depth"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
        )
    }
}

/// Append a bring-up diagnostic to `idtx_visionos.log` in the app's writable temp
/// dir (mirrors the runtime's `diag`), so the scene-anchor computation can be
/// inspected from a device log pull. Best-effort.
fn visionos_diag(msg: &str) {
    use std::io::Write as _;
    let path = std::env::temp_dir().join("idtx_visionos.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{msg}");
    }
}

/// Build a render camera for one eye from its located pose + asymmetric FOV —
/// the Metal path's copy of the `OpenXR` `eye_camera`/`perspective_off_center`
/// (that module is `#[cfg(idtx_xr)]`, so it is not compiled on visionOS).
///
/// `anchor` is the scene-placement transform (stage-world → placed-world);
/// premultiplying it into the view matrix displays geometry authored at the world
/// origin at the anchored location in front of the viewer, without moving the
/// actual scene data. Pass [`Mat4::IDENTITY`] for no placement.
///
/// `recenter` is the play-space recenter offset (stage-space ground vector)
/// subtracted from the per-eye position. The pawn root carries `+recenter`
/// (baked by `xr_player_move`, so it also moves the anchor via the camera prim);
/// subtracting it from the raw head here cancels the recenter for the view
/// (`H' = T(−s)·H`), exactly as the `OpenXR` `eye_camera` does — the headset view
/// is invariant to a recenter snap while the static capsule relocates under the
/// head.
fn eye_camera(view: &XrView, anchor: Mat4, recenter: Vec3) -> RenderCameraDesc {
    let pos = view.pose.position - recenter;
    let rot = view.pose.orientation;
    let view_mat = Mat4::from_rotation_translation(rot, pos).inverse() * anchor;
    let fov = view.fov;
    let projection = perspective_off_center(
        fov.angle_left.tan(),
        fov.angle_right.tan(),
        fov.angle_up.tan(),
        fov.angle_down.tan(),
    );
    RenderCameraDesc {
        prim_path: "visionos:eye".to_owned(),
        view: view_mat,
        projection,
        clip_near: NEAR,
        clip_far: FAR,
        fov_horizontal: (fov.angle_right - fov.angle_left).abs(),
        exposure_ev: 0.0,
    }
}

/// The viewer anchor when bound to a scene (local-player) camera:
/// `(basis * camera_world).inverse()`.
///
/// `basis` is the stage→render `projection_basis`; `camera_world` is the camera
/// prim's USD-world transform. Premultiplied into the view matrix by
/// [`eye_camera`], it maps the ARKit tracking origin onto the camera's world
/// transform, so an identity head pose renders from the camera position and head
/// motion adds the eye offset ("player rig + headset offset"). For an identity
/// rig under an identity basis this is the identity, placing the viewer at the
/// scene origin.
pub(crate) fn scene_camera_anchor(basis: Mat4, camera_world: Mat4) -> Mat4 {
    (basis * camera_world).inverse()
}

/// Compute the scene-anchor transform from the stage AABB (USD space; transformed
/// to render space via `basis`) and the first frame's eye pose. Places the AABB
/// centre a comfortable distance ahead of the viewer at eye height, room-anchored
/// (fixed thereafter so the user can walk around the model). Real scale is
/// preserved. When the scene reports no bounds (`usd_aabb` is `None` — e.g. the
/// standalone hydrator didn't compute `BoundsComp`), it still places the **world
/// origin** ~2.5 m ahead at eye height so origin-authored geometry isn't rendered
/// at the user's feet. Always returns a placement.
fn compute_visionos_anchor(
    usd_aabb: Option<([f32; 3], [f32; 3])>,
    basis: Mat4,
    first_eye: &XrView,
) -> Mat4 {
    // Scene centre + radius in render space; fall back to the origin with an
    // assumed ~1 m radius when bounds are unknown.
    let (center, radius) = match usd_aabb {
        Some((usd_min, usd_max)) => {
            let min = (basis * Vec4::new(usd_min[0], usd_min[1], usd_min[2], 1.0)).truncate();
            let max = (basis * Vec4::new(usd_max[0], usd_max[1], usd_max[2], 1.0)).truncate();
            ((min + max) * 0.5, ((max - min).length() * 0.5).max(0.1))
        }
        None => (Vec3::ZERO, 1.0),
    };

    // Horizontal forward from the eye's orientation (eye ≈ head orientation).
    let fwd = first_eye.pose.orientation * Vec3::NEG_Z;
    let mut fwd_h = Vec3::new(fwd.x, 0.0, fwd.z);
    if fwd_h.length_squared() < 1e-4 {
        fwd_h = Vec3::NEG_Z;
    }
    let fwd_h = fwd_h.normalize();

    // Place the model comfortably ahead at eye height: the AABB centre about one
    // metre beyond the bounding sphere so it reads as a whole and can be walked
    // toward, never closer than 2 m.
    let distance = (radius + 1.0).max(2.0);
    let eye = first_eye.pose.position;
    let target = Vec3::new(
        eye.x + fwd_h.x * distance,
        eye.y,
        eye.z + fwd_h.z * distance,
    );
    // Translate the AABB centre (or origin) to the target (real scale preserved).
    Mat4::from_translation(target - center)
}

/// Right-handed, `z ∈ [0, 1]` off-center perspective from the four FOV tangents
/// (near [`NEAR`], far [`FAR`]). Identical to the `OpenXR` path's
/// `perspective_off_center`; the depth convention is locked by that module's
/// `depth_range_matches_projection_convention` test.
fn perspective_off_center(tan_l: f32, tan_r: f32, tan_u: f32, tan_d: f32) -> Mat4 {
    let w = tan_r - tan_l;
    let h = tan_u - tan_d;
    let a00 = 2.0 / w;
    let a11 = 2.0 / h;
    let a20 = (tan_r + tan_l) / w;
    let a21 = (tan_u + tan_d) / h;
    let a22 = -FAR / (FAR - NEAR);
    let a32 = -(FAR * NEAR) / (FAR - NEAR);
    Mat4::from_cols(
        Vec4::new(a00, 0.0, 0.0, 0.0),
        Vec4::new(0.0, a11, 0.0, 0.0),
        Vec4::new(a20, a21, a22, -1.0),
        Vec4::new(0.0, 0.0, a32, 0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use idtx_platform_xr::view::{Fov, HmdPose, XrView};

    #[cfg(all(feature = "authoring", feature = "metrics"))]
    #[test]
    #[ignore = "requires OpenUSD + headless GPU"]
    fn every_session_flushes_its_final_metric_window_on_drop() {
        let directory = crate::xr_metrics::metrics_output_dir();
        let paths = || std::fs::read_dir(&directory).unwrap().filter_map(Result::ok)
            .map(|e| e.path()).filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("idtx-xr-metrics-idtx_hud_camera-"))
            .collect::<std::collections::HashSet<_>>();
        let before = paths();
        for _ in 0..2 {
            let (mut session, _, _) = presentation_session();
            session.xr_metrics.record(crate::xr_metrics::XrFrameTimings { total_ns: 42, ..Default::default() });
            drop(session);
        }
        let after = paths();
        let new: Vec<_> = after.difference(&before).collect();
        assert_eq!(new.len(), 2, "each discarded session must flush independently");
        for path in new {
            let value = std::fs::read_to_string(path).unwrap();
            assert!(value.contains("\"frames_recorded\": 1"));
            std::fs::remove_file(path).unwrap();
        }
    }

    #[cfg(feature = "authoring")]
    fn presentation_session() -> (StudioSession, wgpu::Device, wgpu::Queue) {
        idtx_usd_extension::install().unwrap();
        let (device, queue) = pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()).await.unwrap().request_device(&wgpu::DeviceDescriptor::default()).await.unwrap()
        });
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/usd/behavior/idtx_hud_camera.usda");
        let session = StudioSession::open_with_gpu(path, device.clone(), queue.clone(), [32, 32],
            crate::TextureDecodeConfig::default(), crate::RenderWorldUnitScale::METRE).unwrap();
        (session, device, queue)
    }

    #[cfg(feature = "authoring")]
    #[test]
    #[ignore = "requires OpenUSD + headless GPU"]
    fn presentation_hud_is_present_and_value_updates_do_not_reextract_geometry() {
        let (mut session, _, _) = presentation_session();
        session.apply_guest_hud_attach_for_test("/World/Camera", "Crosshair", "crosshair.png", [16, 16], 1.0);
        session.ensure_visionos_snapshot();
        assert_eq!(session.last_snapshot.as_ref().unwrap().hud_elements.len(), 1);
        let serial = session.last_snapshot.as_ref().unwrap().serial;
        session.service.author_session_property("/World/Camera/Crosshair", "idtx:hud:opacity", &idtx_usd::PropertyValue::F32(0.25)).unwrap();
        session.tick().unwrap();
        let maintenance = session.ensure_visionos_snapshot();
        assert!(!maintenance.extracted, "HUD-only changes must preserve mesh caches");
        assert_eq!(session.last_snapshot.as_ref().unwrap().serial, serial);
        assert!((session.last_snapshot.as_ref().unwrap().hud_elements[0].opacity - 0.25).abs() < 1e-6);
    }

    #[cfg(feature = "authoring")]
    #[test]
    #[ignore = "requires OpenUSD + headless GPU"]
    fn presentation_prefetches_late_udim_tiles_and_keeps_transform_fast_path() {
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        let (mut session, device, queue) = presentation_session();
        session.ensure_visionos_snapshot();
        let reads = Arc::new(AtomicUsize::new(0));
        let count = reads.clone();
        session.renderer = idtx_render::Renderer::from_device_with_reader(device, queue, Arc::new(move |path| {
            if path.contains("albedo.") { count.fetch_add(1, Ordering::Relaxed); }
            std::fs::read(path).ok()
        }), crate::TextureDecodeConfig::default());
        session.define_prim("/World/Late", "Xform").unwrap();
        let asset = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/usd/udim/udim_plane.usda");
        session.add_reference("/World/Late", asset.to_str().unwrap(), None).unwrap();
        session.author_transform("/World/Late", [0.0; 3], [0.0; 3], [1.0; 3]).unwrap();
        session.tick().unwrap();
        session.ensure_visionos_snapshot();
        let loaded = reads.load(Ordering::Relaxed);
        assert!(loaded >= 3, "late material never reached UDIM prefetch");
        let serial = session.last_snapshot.as_ref().unwrap().serial;
        session.author_transform("/World/Late", [1.0, 0.0, 0.0], [0.0; 3], [1.0; 3]).unwrap();
        session.tick().unwrap();
        assert!(!session.ensure_visionos_snapshot().extracted);
        assert_eq!(session.last_snapshot.as_ref().unwrap().serial, serial);
        assert_eq!(reads.load(Ordering::Relaxed), loaded);
    }

    #[cfg(feature = "authoring")]
    #[test]
    #[ignore = "requires OpenUSD + headless GPU"]
    fn presentation_shares_highlights_cues_and_widget_value_patches() {
        let (mut session, _, _) = presentation_session();
        session.define_prim("/World/Box", "Cube").unwrap();
        session.define_prim("/World/Widgets", "Xform").unwrap();
        let asset = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/usd/widgets/widget_fetch_value.usda");
        session.add_reference("/World/Widgets", asset.to_str().unwrap(), None).unwrap();
        session.tick().unwrap();
        session.behavior_highlights = [crate::HighlightStyle::Box, crate::HighlightStyle::Tint, crate::HighlightStyle::Outline]
            .into_iter().map(|style| crate::HighlightEntry { path: "/World/Box".into(), color: [1.0, 0.0, 0.0, 0.5], style, thickness: 3.0 }).collect();
        session.behavior_overlay_lines = crate::assemble_behavior_overlay_lines(&[], &[idtx_behavior::WidgetMarker {
            position: glam::Vec3::ZERO, normal: glam::Vec3::Z,
        }], 1.0);
        let cues = session.behavior_overlay_lines.len();
        assert!(cues > 0);
        session.ensure_visionos_snapshot();
        let serial = session.last_snapshot.as_ref().unwrap().serial;
        let hash = session.last_snapshot.as_ref().unwrap().widget_panels[0].content_hash;
        for desktop in [false, true] {
            let value = if desktop { "17.5" } else { "42.0" };
            session.apply_guest_widget_value_edit_for_test("/World/Widgets/Readout", "Row/Value", value);
            if desktop { session.render_one_frame(false); } else { assert!(!session.ensure_visionos_snapshot().extracted); }
            let snapshot = session.last_snapshot.as_ref().unwrap();
            assert_eq!(snapshot.serial, serial);
            assert_ne!(snapshot.widget_panels[0].content_hash, hash);
            assert_eq!(snapshot.highlight_tints.len(), 1);
            assert_eq!(snapshot.highlight_outlines.len(), 1);
            assert_eq!(session.presentation_overlay_lines.len(), cues + 12);
        }
        session.behavior_highlights.clear();
        session.behavior_overlay_lines.clear();
        session.ensure_visionos_snapshot();
        assert!(session.presentation_overlay_lines.is_empty());
        assert!(session.last_snapshot.as_ref().unwrap().highlight_tints.is_empty());
        assert!(session.last_snapshot.as_ref().unwrap().highlight_outlines.is_empty());
    }

    /// The per-eye→`XrFrameTimings` assembly must place each eye's renderer
    /// counters/timings in the matching slot, carry the present cost into
    /// `eye_swapchain_copy_ns`, surface draw-call counts, and leave the
    /// `OpenXR`-only pacing fields zero. Pure — no GPU needed.
    #[cfg(feature = "metrics")]
    #[test]
    fn frame_timings_builder_maps_both_eyes() {
        use idtx_render::metrics::RenderFrameTimings;
        let left = RenderFrameTimings {
            total_ns: 5_000_000,
            draw_calls: 400,
            pipeline_switches: 30,
            bind_group_sets: 800,
            mesh_count: 350,
            instance_count: 12,
            merged_batch_count: 5,
            cache_misses: 3,
            evictions: 1,
            draw_list_build_ns: 1_000_000,
            gpu_passes_ns: 3_000_000,
            gpu_pbr_ns: 6_000_000,
            gpu_shadow_ns: 900_000,
            gpu_tonemap_ns: 400_000,
            gpu_wait_ns: 7_000_000,
            ..Default::default()
        };
        let right = RenderFrameTimings {
            total_ns: 5_200_000,
            draw_calls: 410,
            mesh_count: 352,
            ..left.clone()
        };
        let eye = VisionosEyeMetrics {
            frames: [left, right],
            present_ns: [800_000, 850_000],
        };
        let maint = SnapshotMaint {
            extracted: true,
            full_extract_ns: 2_500_000,
            patch_widget_ns: 123_456,
            ..Default::default()
        };
        let t = build_visionos_frame_timings(&eye, 12_000_000, 2_500_000, &maint);

        assert_eq!(t.total_ns, 12_000_000);
        assert_eq!(t.extract_ns, 2_500_000);
        assert!(t.extracted_this_frame);
        assert_eq!(t.full_extract_ns, 2_500_000);
        assert_eq!(t.patch_widget_ns, 123_456);
        assert_eq!(t.eye_render_ns, [5_000_000, 5_200_000]);
        assert_eq!(t.eye_draw_calls, [400, 410]);
        assert_eq!(t.eye_pipeline_switches[0], 30);
        assert_eq!(t.eye_bind_group_sets[0], 800);
        assert_eq!(t.eye_mesh_count, [350, 352]);
        assert_eq!(t.eye_merged_batch_count[0], 5);
        assert_eq!(t.eye_gpu_pbr_ns[1], 6_000_000);
        assert_eq!(t.eye_swapchain_copy_ns, [800_000, 850_000]);
        // CompositorServices has no OpenXR pacing — those stay default/zero.
        assert_eq!(t.wait_frame_ns, 0);
        assert_eq!(t.frame_interval_ns, 0);
    }

    /// Build the depth-upscale pipeline on a real Metal device so naga validates
    /// the WGSL (shader errors surface at pipeline creation, not `cargo check`).
    /// The scene render path cannot run headless (needs `CompositorServices`
    /// drawables), so this is the host-side guard for the render-scale depth path.
    /// Skips if no GPU adapter is available (CI software runners).
    #[test]
    fn depth_upscale_pipeline_wgsl_is_valid() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions::default(),
        )) else {
            eprintln!("no GPU adapter; skipping depth-upscale WGSL validation");
            return;
        };
        let Ok((device, _queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            eprintln!("no device; skipping");
            return;
        };
        // Two eyes at a scaled render size; `true` = bindable (sampled by upscale).
        let depth_resolve: Vec<_> = (0..2)
            .map(|_| make_depth_resolve_target(&device, 60, 40, true))
            .collect();
        // Constructing this validates DEPTH_UPSCALE_WGSL (naga) + the pipeline /
        // bind-group layout; a shader or layout error panics here.
        let up = DepthUpscale::new(&device, &depth_resolve, 60, 40, 100, 66);
        assert_eq!(up.bind_groups.len(), 2);
    }

    /// An eye view at `pos` with identity orientation and a trivial symmetric FOV.
    fn eye_at(pos: Vec3) -> XrView {
        XrView {
            pose: HmdPose {
                position: pos,
                orientation: glam::Quat::IDENTITY,
                timestamp: 0.0,
            },
            fov: Fov {
                angle_left: -0.5,
                angle_right: 0.5,
                angle_up: 0.5,
                angle_down: -0.5,
            },
        }
    }

    /// An identity rig under an identity basis anchors the viewer at the scene
    /// origin: the anchor is the identity (viewer spawns at origin, not pushed
    /// away like the AABB fallback).
    #[test]
    fn identity_rig_anchors_at_origin() {
        let anchor = scene_camera_anchor(Mat4::IDENTITY, Mat4::IDENTITY);
        assert!(anchor.abs_diff_eq(Mat4::IDENTITY, 1e-6));
    }

    /// With the viewer bound to a camera translated to `cam_pos`, an identity head
    /// pose must render the stage origin from `cam_pos`: the composed view matrix
    /// maps the stage-origin point to `-cam_pos` in eye space (eye looks down −Z,
    /// so a point at the camera position sits at the eye). Verifies the
    /// `view = H.inverse() * anchor` composition in `eye_camera`.
    #[test]
    fn head_pose_composes_with_player_camera() {
        let cam_pos = Vec3::new(2.0, 1.0, -3.0);
        let camera_world = Mat4::from_translation(cam_pos);
        let anchor = scene_camera_anchor(Mat4::IDENTITY, camera_world);

        // Identity head pose (eye at ARKit origin, looking −Z).
        let cam = eye_camera(&eye_at(Vec3::ZERO), anchor, Vec3::ZERO);
        let origin_in_eye = cam.view.transform_point3(Vec3::ZERO);
        // The stage origin is `cam_pos` behind the camera: at `-cam_pos` in eye space.
        assert!(
            origin_in_eye.abs_diff_eq(-cam_pos, 1e-5),
            "stage origin in eye space = {origin_in_eye:?}, expected {:?}",
            -cam_pos
        );

        // A stage point AT the camera position lands at the eye (origin of eye space).
        let at_cam = cam.view.transform_point3(cam_pos);
        assert!(
            at_cam.abs_diff_eq(Vec3::ZERO, 1e-5),
            "camera-position point in eye space = {at_cam:?}, expected origin"
        );
    }

    /// A play-space recenter must leave the immersive view **unchanged**, same as
    /// the `OpenXR` path: the pawn root (hence the camera-prim anchor) bakes
    /// `+R(yaw)·s` and the eye subtracts `s`, so `M·T(−s)·H = M0·H`. The view is
    /// identical before and after a snap, for a non-zero yaw.
    #[test]
    fn recenter_is_invariant_for_the_immersive_view() {
        let pos = Vec3::new(3.0, 0.0, -2.0); // locomotion
        let yaw = 0.6_f32;
        let r_yaw = Mat4::from_rotation_y(yaw);
        let s = Vec3::new(0.5, 0.0, -0.3); // recenter ground offset

        // Camera-prim world = the pawn root (identity camera-local, hmd_camera_sync
        // no-ops on visionOS). Pre-recenter vs baked.
        let m0 = Mat4::from_translation(pos) * r_yaw;
        let baked = glam::Quat::from_rotation_y(yaw) * s;
        let m = Mat4::from_translation(pos + baked) * r_yaw;

        let head = eye_at(Vec3::new(0.2, 1.6, -0.1));
        // Recentered: anchor from the baked root, eye subtracts s.
        let v_recentered = eye_camera(&head, scene_camera_anchor(Mat4::IDENTITY, m), s).view;
        // Pre-recenter: anchor from the base root, no eye subtraction.
        let v_base = eye_camera(&head, scene_camera_anchor(Mat4::IDENTITY, m0), Vec3::ZERO).view;
        assert!(
            v_recentered.abs_diff_eq(v_base, 1e-4),
            "recenter must not move the immersive view:\n{v_recentered:?}\nvs\n{v_base:?}"
        );
    }
}
