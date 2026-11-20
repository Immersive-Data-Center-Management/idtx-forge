//! Selection overlay — corner-tick bounding-box visualisation.
//!
//! Rendered on top of the LDR `color_view` after tonemapping with no depth
//! buffer (the studio's MSAA depth cannot be resolved cheaply).  Lines are
//! drawn in two passes:
//!
//! 1. **xray** — alpha 0.18, visible even when occluded by geometry.
//! 2. **crisp** — alpha 1.0, fully opaque.

use crate::convert::u32_sat;
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

// ── Vertex layout ─────────────────────────────────────────────────────────────

/// A single overlay line segment: two world-space endpoints, each with RGBA colour.
#[derive(Debug, Clone, Copy)]
pub struct OverlayLine {
    pub a: [f32; 3],
    pub b: [f32; 3],
    /// RGBA colour for both endpoints (alpha is scaled per-pass).
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct OverlayVertex {
    position: [f32; 3],
    _pad: f32,
    color: [f32; 4],
}

const VERTEX_STRIDE: u64 = std::mem::size_of::<OverlayVertex>() as u64;

// ── Frame uniform ─────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameUniforms {
    view_proj: [[f32; 4]; 4],
}

// ── OverlayPass ───────────────────────────────────────────────────────────────

/// Holds the overlay pipeline state.  Cheap to create; rebuild on format change.
pub struct OverlayPass {
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
}

impl std::fmt::Debug for OverlayPass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayPass").finish_non_exhaustive()
    }
}

impl OverlayPass {
    /// Creates the overlay pipeline targeting `output_format` (must be `COLOR_FORMAT`).
    #[must_use]
    pub fn new(device: &wgpu::Device, output_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Overlay Shader"),
            source: wgpu::ShaderSource::Wgsl(OVERLAY_WGSL.into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Overlay BGL"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Overlay PL"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Overlay Pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: VERTEX_STRIDE,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16, // after [f32;3] + pad f32
                            shader_location: 1,
                        },
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: output_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self { pipeline, bgl }
    }

    /// Renders `lines` onto `target` (the LDR colour view) using `view_proj` for
    /// world-to-clip space transform.  Draws two passes: xray (α=0.18) then crisp (α=1.0).
    pub fn render(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        view_proj: glam::Mat4,
        lines: &[OverlayLine],
    ) {
        if lines.is_empty() {
            return;
        }

        // Frame uniform
        let frame_u = FrameUniforms {
            view_proj: view_proj.to_cols_array_2d(),
        };
        let frame_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Overlay Frame UB"),
            contents: bytemuck::bytes_of(&frame_u),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let frame_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Overlay Frame BG"),
            layout: &self.bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: frame_buf.as_entire_binding(),
            }],
        });

        // Build vertex buffer for the xray pass (alpha * 0.18) and crisp pass (alpha * 1.0).
        let n_verts = lines.len() * 2;
        let mut xray_verts: Vec<OverlayVertex> = Vec::with_capacity(n_verts);
        let mut crisp_verts: Vec<OverlayVertex> = Vec::with_capacity(n_verts);

        for line in lines {
            let xray_a = [
                line.color[0],
                line.color[1],
                line.color[2],
                line.color[3] * 0.18,
            ];
            let crisp_a = line.color;
            for &pos in &[line.a, line.b] {
                xray_verts.push(OverlayVertex {
                    position: pos,
                    _pad: 0.0,
                    color: xray_a,
                });
                crisp_verts.push(OverlayVertex {
                    position: pos,
                    _pad: 0.0,
                    color: crisp_a,
                });
            }
        }

        let xray_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Overlay XRay VBO"),
            contents: bytemuck::cast_slice(&xray_verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let crisp_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Overlay Crisp VBO"),
            contents: bytemuck::cast_slice(&crisp_verts),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let vertex_count = u32_sat(lines.len() * 2);

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Overlay Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load, // overlay on top of tonemapped output
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &frame_bg, &[]);

        // xray pass — semi-transparent, drawn first
        pass.set_vertex_buffer(0, xray_buf.slice(..));
        pass.draw(0..vertex_count, 0..1);

        // crisp pass — fully opaque
        pass.set_vertex_buffer(0, crisp_buf.slice(..));
        pass.draw(0..vertex_count, 0..1);
    }
}

// ── Depth-tested runtime overlay pass ─────────────────────────────────

/// Per-frame GPU resources for depth-tested overlay drawing.
///
/// Build with [`DepthOverlayPass::prepare`] **before** starting the render pass
/// so the vertex buffer outlives the pass and satisfies wgpu buffer-slice
/// lifetimes (the same discipline as [`AxisGpuData`]).
pub struct DepthOverlayGpuData {
    /// Vertex buffer of `OverlayVertex` (two per line).
    pub vbuf: wgpu::Buffer,
    /// Frame bind group (view-projection uniform).
    pub frame_bg: wgpu::BindGroup,
    /// Vertex count to draw.
    pub vertex_count: u32,
}

/// Depth-tested overlay line pipeline for **behavior-emitted** overlay geometry
/// (`draw_line`/`draw_arc`, ribbon builders).
///
/// Unlike [`OverlayPass`] — which draws selection ticks on top of the
/// tonemapped LDR image with the depth buffer discarded — this pass runs inside
/// the MSAA HDR pass with depth **read** (no write), so overlay geometry a
/// gameplay behaviour draws (a teleport arc, a target reticle) is correctly
/// occluded by scene geometry in front of it. Depth-read/no-write matches
/// [`AxisPass`]. The renderer wires it after opaque geometry, before tonemap.
pub struct DepthOverlayPass {
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
}

impl std::fmt::Debug for DepthOverlayPass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DepthOverlayPass").finish_non_exhaustive()
    }
}

impl DepthOverlayPass {
    /// Create the depth-tested overlay pipeline targeting the MSAA HDR
    /// `hdr_format` with `depth_format` (depth-read, no depth-write) at
    /// `msaa_samples`.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        hdr_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
        msaa_samples: u32,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Depth Overlay Shader"),
            source: wgpu::ShaderSource::Wgsl(OVERLAY_WGSL.into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Depth Overlay BGL"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Depth Overlay PL"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Depth Overlay Pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: VERTEX_STRIDE,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16,
                            shader_location: 1,
                        },
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: hdr_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth_format,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: msaa_samples,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });

        Self { pipeline, bgl }
    }

    /// Build per-frame GPU buffers for `lines`. Call **before** the render pass
    /// starts. Returns `None` when there is nothing to draw.
    #[must_use]
    pub fn prepare(
        &self,
        device: &wgpu::Device,
        view_proj: glam::Mat4,
        lines: &[OverlayLine],
    ) -> Option<DepthOverlayGpuData> {
        if lines.is_empty() {
            return None;
        }

        let frame_u = FrameUniforms {
            view_proj: view_proj.to_cols_array_2d(),
        };
        let frame_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Depth Overlay Frame UB"),
            contents: bytemuck::bytes_of(&frame_u),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let frame_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Depth Overlay Frame BG"),
            layout: &self.bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: frame_buf.as_entire_binding(),
            }],
        });

        let mut verts: Vec<OverlayVertex> = Vec::with_capacity(lines.len() * 2);
        for line in lines {
            for &pos in &[line.a, line.b] {
                verts.push(OverlayVertex {
                    position: pos,
                    _pad: 0.0,
                    color: line.color,
                });
            }
        }
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Depth Overlay VBO"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });

        Some(DepthOverlayGpuData {
            vbuf,
            frame_bg,
            vertex_count: u32_sat(verts.len()),
        })
    }

    /// Record the overlay draw into an already-open render pass whose color
    /// attachment is the MSAA HDR target and whose depth attachment is the
    /// scene depth buffer (loaded, read-only). `data` comes from
    /// [`Self::prepare`].
    pub fn draw<'p>(&'p self, pass: &mut wgpu::RenderPass<'p>, data: &'p DepthOverlayGpuData) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &data.frame_bg, &[]);
        pass.set_vertex_buffer(0, data.vbuf.slice(..));
        pass.draw(0..data.vertex_count, 0..1);
    }
}

// ── Corner-tick helper ────────────────────────────────────────────────────────
/// Generates Blender-style corner-tick lines for a world-space AABB.
///
/// Pushes 24 [`OverlayLine`]s into `out` (3 ticks × 8 corners).
/// `tick_fraction` controls tick length as a fraction of the box size per axis;
/// defaults to 0.25.
pub fn push_aabb_corner_ticks(
    out: &mut Vec<OverlayLine>,
    min: [f32; 3],
    max: [f32; 3],
    color: [f32; 4],
    tick_fraction: f32,
) {
    let span = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];

    // Clamp each axis tick so it never exceeds the box: nominal `fraction`, capped
    // at half the axis span (ticks from opposite corners never cross), with a small
    // fallback floor itself capped at half the span so tiny/flat boxes don't
    // overshoot. A zero-span axis yields a zero-length tick (no tick on a flat axis).
    let fallback = 0.02_f32;
    let tick = [
        (span[0] * tick_fraction)
            .min(span[0] * 0.5)
            .max(fallback.min(span[0] * 0.5)),
        (span[1] * tick_fraction)
            .min(span[1] * 0.5)
            .max(fallback.min(span[1] * 0.5)),
        (span[2] * tick_fraction)
            .min(span[2] * 0.5)
            .max(fallback.min(span[2] * 0.5)),
    ];

    let corners: [[f32; 3]; 8] = [
        [min[0], min[1], min[2]],
        [max[0], min[1], min[2]],
        [min[0], max[1], min[2]],
        [max[0], max[1], min[2]],
        [min[0], min[1], max[2]],
        [max[0], min[1], max[2]],
        [min[0], max[1], max[2]],
        [max[0], max[1], max[2]],
    ];

    for corner in corners {
        // exact-sentinel: corner built from these exact min/max components.
        #[allow(clippy::float_cmp)]
        let dx = if corner[0] == min[0] {
            tick[0]
        } else {
            -tick[0]
        };
        // exact-sentinel: corner built from these exact min/max components.
        #[allow(clippy::float_cmp)]
        let dy = if corner[1] == min[1] {
            tick[1]
        } else {
            -tick[1]
        };
        // exact-sentinel: corner built from these exact min/max components.
        #[allow(clippy::float_cmp)]
        let dz = if corner[2] == min[2] {
            tick[2]
        } else {
            -tick[2]
        };

        out.push(OverlayLine {
            a: corner,
            b: [corner[0] + dx, corner[1], corner[2]],
            color,
        });
        out.push(OverlayLine {
            a: corner,
            b: [corner[0], corner[1] + dy, corner[2]],
            color,
        });
        out.push(OverlayLine {
            a: corner,
            b: [corner[0], corner[1], corner[2] + dz],
            color,
        });
    }
}

/// Generates Blender-style corner-tick lines for an oriented bounding box (OBB).
///
/// `corners` must be the 8 corners of the OBB in render world space, ordered by
/// bit-index: bit 0 = X axis direction, bit 1 = Y axis direction, bit 2 = Z axis
/// direction (same convention as `push_aabb_corner_ticks`).  The adjacency of
/// corner `i` is `i^1`, `i^2`, `i^4`.
///
/// Pushes 24 [`OverlayLine`]s into `out` (3 ticks × 8 corners).
pub fn push_obb_corner_ticks(
    out: &mut Vec<OverlayLine>,
    corners: &[[f32; 3]; 8],
    color: [f32; 4],
    tick_fraction: f32,
) {
    let fallback = 0.02_f32;
    for i in 0usize..8 {
        let c = corners[i];
        // Each corner has 3 adjacent corners at i^1, i^2, i^4.
        for bit in [1usize, 2, 4] {
            let adj = corners[i ^ bit];
            let edge = [adj[0] - c[0], adj[1] - c[1], adj[2] - c[2]];
            let len = (edge[0] * edge[0] + edge[1] * edge[1] + edge[2] * edge[2]).sqrt();
            if len < 1e-6 {
                continue;
            }
            // Clamp: nominal `fraction`, capped at half the edge so opposite-corner
            // ticks never cross, with a fallback floor that is itself capped at half
            // the edge so a tiny edge is never overshot past its neighbouring corner.
            let tick_len = (len * tick_fraction)
                .min(len * 0.5)
                .max(fallback.min(len * 0.5));
            let dir = [edge[0] / len, edge[1] / len, edge[2] / len];
            let b = [
                c[0] + dir[0] * tick_len,
                c[1] + dir[1] * tick_len,
                c[2] + dir[2] * tick_len,
            ];
            out.push(OverlayLine { a: c, b, color });
        }
    }
}

// ── Behavior overlay geometry builders ────────────────────────────────

/// Push a single world-space line segment (`start`→`end`) in `color`.
///
/// The trivial builder behind the behavior `draw_line` overlay command;
/// batched into the same [`OverlayLine`] stream as selection ticks.
pub fn push_line(out: &mut Vec<OverlayLine>, start: [f32; 3], end: [f32; 3], color: [f32; 4]) {
    out.push(OverlayLine {
        a: start,
        b: end,
        color,
    });
}

/// Push a polyline (a chain of connected segments) through `points` in `color`.
///
/// Fewer than two points push nothing. This is the shared primitive the arc /
/// ribbon builders lower onto.
pub fn push_polyline(out: &mut Vec<OverlayLine>, points: &[[f32; 3]], color: [f32; 4]) {
    for seg in points.windows(2) {
        out.push(OverlayLine {
            a: seg[0],
            b: seg[1],
            color,
        });
    }
}

/// Sample a ballistic **arc** from `start` along `dir` (need not be normalized)
/// at initial `speed` (m/s) under `gravity` (m/s², positive = downward on +Y)
/// and push it as a polyline in `color`.
///
/// Sampling stops at the first crossing of `ground_y` (inclusive of the landing
/// point) or after `segments` steps, whichever comes first — mirroring
/// `idtx_platform_xr::teleport_arc` so the drawn arc matches where a teleport
/// behaviour will actually land. Returns the number of [`OverlayLine`]s pushed.
///
/// This is the render-side twin of the platform sampler: `idtx-render` sits
/// below `idtx-platform-xr` in the crate graph and cannot call it, so the
/// closed-form ballistic step is duplicated here (both are covered by tests).
pub fn push_arc(
    out: &mut Vec<OverlayLine>,
    start: [f32; 3],
    dir: [f32; 3],
    speed: f32,
    gravity: f32,
    ground_y: f32,
    dt: f32,
    segments: usize,
    color: [f32; 4],
) -> usize {
    let points = sample_arc(start, dir, speed, gravity, ground_y, dt, segments);
    let before = out.len();
    push_polyline(out, &points, color);
    out.len() - before
}

/// Sample a ballistic arc into its points (see [`push_arc`] for the trajectory
/// definition), so a caller can lower them onto either the line polyline or the
/// thick [`push_ribbon`] stroke. Returns at most `segments + 1` points; an empty
/// vec for `segments == 0`.
#[must_use]
pub fn sample_arc(
    start: [f32; 3],
    dir: [f32; 3],
    speed: f32,
    gravity: f32,
    ground_y: f32,
    dt: f32,
    segments: usize,
) -> Vec<[f32; 3]> {
    if segments == 0 {
        return Vec::new();
    }
    let start = glam::Vec3::from(start);
    let v0 = glam::Vec3::from(dir).normalize_or_zero() * speed;
    let mut points: Vec<[f32; 3]> = Vec::with_capacity(segments + 1);
    let mut prev = start;
    points.push(prev.into());
    let mut t = 0.0_f32;
    for _ in 0..segments {
        t += dt;
        let p = start + v0 * t - glam::Vec3::Y * (0.5 * gravity * t * t);
        if p.y <= ground_y && prev.y > ground_y {
            let span = prev.y - p.y;
            let frac = if span > f32::EPSILON {
                ((prev.y - ground_y) / span).clamp(0.0, 1.0)
            } else {
                0.0
            };
            points.push(prev.lerp(p, frac).into());
            break;
        }
        points.push(p.into());
        prev = p;
    }
    points
}

/// Push a flat **ribbon** (a thick stroke) along `points`, `width` metres wide,
/// lying in the plane whose normal is `up`, in `color`.
///
/// Emitted as its two offset rails plus a rung at each sample — a wireframe
/// stand-in for a filled strip that reuses the line pipeline (no dedicated
/// triangle overlay pass). Fewer than two points, a near-zero `width`, or an
/// `up` parallel to every segment push nothing. Returns the number of
/// [`OverlayLine`]s pushed.
pub fn push_ribbon(
    out: &mut Vec<OverlayLine>,
    points: &[[f32; 3]],
    width: f32,
    up: [f32; 3],
    color: [f32; 4],
) -> usize {
    if points.len() < 2 || width.abs() < 1e-6 {
        return 0;
    }
    let up = glam::Vec3::from(up).normalize_or_zero();
    // degenerate guard after normalize_or_zero returns bit-exact zero.
    #[allow(clippy::float_cmp)]
    if up == glam::Vec3::ZERO {
        return 0;
    }
    let half = width * 0.5;
    let before = out.len();

    // Compute the two offset rails per sample, using the perpendicular to the
    // local segment direction in the ribbon plane.
    let mut left: Vec<glam::Vec3> = Vec::with_capacity(points.len());
    let mut right: Vec<glam::Vec3> = Vec::with_capacity(points.len());
    for i in 0..points.len() {
        let p = glam::Vec3::from(points[i]);
        // Local tangent: forward difference, backward at the last point.
        let tangent = if i + 1 < points.len() {
            glam::Vec3::from(points[i + 1]) - p
        } else {
            p - glam::Vec3::from(points[i - 1])
        };
        let side = tangent.cross(up).normalize_or_zero();
        // degenerate guard after normalize_or_zero returns bit-exact zero.
        #[allow(clippy::float_cmp)]
        if side == glam::Vec3::ZERO {
            // Segment parallel to `up`: skip this rail pair, keep the rung chain
            // continuous by reusing the centre point.
            left.push(p);
            right.push(p);
        } else {
            left.push(p + side * half);
            right.push(p - side * half);
        }
    }

    // Two rails as polylines + a rung joining the rails at each sample.
    for i in 0..points.len() {
        if i + 1 < points.len() {
            out.push(OverlayLine {
                a: left[i].into(),
                b: left[i + 1].into(),
                color,
            });
            out.push(OverlayLine {
                a: right[i].into(),
                b: right[i + 1].into(),
                color,
            });
        }
        out.push(OverlayLine {
            a: left[i].into(),
            b: right[i].into(),
            color,
        });
    }
    out.len() - before
}

/// Pushes a Blender-style octahedral bone wireframe spanning `head`→`tail`.
///
/// The octahedron has its widest ring near the head (parent joint), tapering to
/// a point at the tail (child joint) — the conventional skeletal bone glyph.
/// Pushes 12 [`OverlayLine`]s (4 head→ring, 4 ring→tail, 4 ring loop). Degenerate
/// zero-length bones push nothing.
#[cfg(feature = "editor-overlay")]
pub fn push_bone_octahedron(
    out: &mut Vec<OverlayLine>,
    head: [f32; 3],
    tail: [f32; 3],
    color: [f32; 4],
) {
    let h = glam::Vec3::from(head);
    let t = glam::Vec3::from(tail);
    let axis = t - h;
    let len = axis.length();
    if len < 1e-6 {
        return;
    }
    let dir = axis / len;

    // Perpendicular basis: pick a reference not parallel to `dir`.
    let reference = if dir.x.abs() < 0.9 {
        glam::Vec3::X
    } else {
        glam::Vec3::Y
    };
    let side = dir.cross(reference).normalize();
    let up = dir.cross(side).normalize();

    // Ring sits 10% of the way from head to tail, spread by 10% of length.
    let ring_center = h + dir * (len * 0.1);
    let r = len * 0.1;
    let ring = [
        ring_center + side * r,
        ring_center + up * r,
        ring_center - side * r,
        ring_center - up * r,
    ];

    let push = |out: &mut Vec<OverlayLine>, a: glam::Vec3, b: glam::Vec3| {
        out.push(OverlayLine {
            a: a.to_array(),
            b: b.to_array(),
            color,
        });
    };

    for &ring_pt in &ring {
        push(out, h, ring_pt); // head → ring
        push(out, ring_pt, t); // ring → tail
    }
    for i in 0..4 {
        push(out, ring[i], ring[(i + 1) % 4]); // ring loop
    }
}

// ── Axis pass (editor-overlay feature) ───────────────────────────────────────

/// A world-origin axis line for depth-tested rendering in the MSAA HDR pass.
///
/// `b` is the far endpoint; [`AxisPass`] marks it `is_far = 1.0` and the vertex
/// shader pins it to the clip-space far plane, making the line extend to the
/// screen horizon regardless of zoom level.
#[cfg(feature = "editor-overlay")]
#[derive(Debug, Clone, Copy)]
pub struct AxisLine {
    /// Near endpoint (always the world origin).
    pub a: [f32; 3],
    /// Far endpoint — pinned to clip-space far plane by the vertex shader.
    pub b: [f32; 3],
    pub color_a: [f32; 4],
    pub color_b: [f32; 4],
}

#[cfg(feature = "editor-overlay")]
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AxisVertex {
    position: [f32; 3],
    is_far: f32,
    color: [f32; 4],
}

#[cfg(feature = "editor-overlay")]
const AXIS_VERTEX_STRIDE: u64 = std::mem::size_of::<AxisVertex>() as u64;

/// Per-frame GPU resources for axis line drawing.
///
/// Build with [`AxisPass::prepare`] **before** starting the render pass so the
/// vertex buffer outlives the pass and satisfies wgpu buffer-slice lifetimes.
#[cfg(feature = "editor-overlay")]
#[allow(missing_debug_implementations)]
pub struct AxisGpuData {
    pub vbuf: wgpu::Buffer,
    pub frame_bg: wgpu::BindGroup,
    pub vertex_count: u32,
}

/// Depth-tested world-axis line pipeline (MSAA HDR, depth-read, no depth-write).
#[cfg(feature = "editor-overlay")]
pub struct AxisPass {
    pub pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
}

#[cfg(feature = "editor-overlay")]
impl std::fmt::Debug for AxisPass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AxisPass").finish_non_exhaustive()
    }
}

#[cfg(feature = "editor-overlay")]
impl AxisPass {
    pub fn new(device: &wgpu::Device, hdr_format: wgpu::TextureFormat, msaa_samples: u32) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Axis Shader"),
            source: wgpu::ShaderSource::Wgsl(AXIS_WGSL.into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Axis BGL"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Axis PL"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Axis Pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: AXIS_VERTEX_STRIDE,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32,
                            offset: 12,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16,
                            shader_location: 2,
                        },
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: hdr_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: msaa_samples,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });

        Self { pipeline, bgl }
    }

    /// Build per-frame GPU buffers for `lines`.  Call **before** the render pass starts.
    pub fn prepare(
        &self,
        device: &wgpu::Device,
        view_proj: glam::Mat4,
        lines: &[AxisLine],
    ) -> Option<AxisGpuData> {
        if lines.is_empty() {
            return None;
        }

        let frame_u = FrameUniforms {
            view_proj: view_proj.to_cols_array_2d(),
        };
        let frame_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Axis Frame UB"),
            contents: bytemuck::bytes_of(&frame_u),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let frame_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Axis Frame BG"),
            layout: &self.bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: frame_buf.as_entire_binding(),
            }],
        });

        let mut verts: Vec<AxisVertex> = Vec::with_capacity(lines.len() * 2);
        for line in lines {
            verts.push(AxisVertex {
                position: line.a,
                is_far: 0.0,
                color: line.color_a,
            });
            verts.push(AxisVertex {
                position: line.b,
                is_far: 1.0,
                color: line.color_b,
            });
        }
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Axis VBO"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });

        Some(AxisGpuData {
            vbuf,
            frame_bg,
            vertex_count: u32_sat(verts.len()),
        })
    }
}

/// Pushes world-origin axis lines into `out` (6 lines, 2 per axis).
///
/// Positive halves use full alpha; negative halves use lighter desaturated
/// colors at half alpha.  The far endpoint of each line is pinned to the
/// clip-space far plane by [`AxisPass`]'s vertex shader.
#[cfg(feature = "editor-overlay")]
pub fn push_world_axes(out: &mut Vec<AxisLine>) {
    let o = [0.0_f32; 3];
    let far = 1_000_000.0_f32;
    out.push(AxisLine {
        a: o,
        b: [far, 0.0, 0.0],
        color_a: [1.00, 0.00, 0.00, 1.0],
        color_b: [1.00, 0.00, 0.00, 1.0],
    });
    out.push(AxisLine {
        a: o,
        b: [-far, 0.0, 0.0],
        color_a: [1.00, 0.45, 0.45, 0.5],
        color_b: [1.00, 0.45, 0.45, 0.5],
    });
    out.push(AxisLine {
        a: o,
        b: [0.0, far, 0.0],
        color_a: [0.00, 1.00, 0.00, 1.0],
        color_b: [0.00, 1.00, 0.00, 1.0],
    });
    out.push(AxisLine {
        a: o,
        b: [0.0, -far, 0.0],
        color_a: [0.45, 1.00, 0.45, 0.5],
        color_b: [0.45, 1.00, 0.45, 0.5],
    });
    out.push(AxisLine {
        a: o,
        b: [0.0, 0.0, far],
        color_a: [0.20, 0.40, 1.00, 1.0],
        color_b: [0.20, 0.40, 1.00, 1.0],
    });
    out.push(AxisLine {
        a: o,
        b: [0.0, 0.0, -far],
        color_a: [0.55, 0.70, 1.00, 0.5],
        color_b: [0.55, 0.70, 1.00, 0.5],
    });
}

// ── WGSL shaders ─────────────────────────────────────────────────────────────

const OVERLAY_WGSL: &str = r"
struct Frame {
    view_proj: mat4x4<f32>,
}
@group(0) @binding(0) var<uniform> frame: Frame;

struct VertIn {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
}

struct VertOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_main(in: VertIn) -> VertOut {
    var out: VertOut;
    out.clip_pos = frame.view_proj * vec4<f32>(in.position, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertOut) -> @location(0) vec4<f32> {
    return in.color;
}
";

#[cfg(test)]
#[allow(clippy::float_cmp)] // exact-value assertions on known constants
mod builder_tests {
    use super::*;

    const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

    #[test]
    fn line_pushes_one_segment() {
        let mut out = Vec::new();
        push_line(&mut out, [0.0, 0.0, 0.0], [1.0, 2.0, 3.0], WHITE);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].a, [0.0, 0.0, 0.0]);
        assert_eq!(out[0].b, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn polyline_connects_n_points_into_n_minus_one_segments() {
        let mut out = Vec::new();
        push_polyline(
            &mut out,
            &[[0.0; 3], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]],
            WHITE,
        );
        assert_eq!(out.len(), 2);
        let mut empty = Vec::new();
        push_polyline(&mut empty, &[[0.0; 3]], WHITE);
        assert!(empty.is_empty());
    }

    #[test]
    fn arc_lands_on_ground_and_matches_platform_sampler_shape() {
        // Shoot forward+up from 1.5 m; it must arc down and end on the ground.
        let mut out = Vec::new();
        let n = push_arc(
            &mut out,
            [0.0, 1.5, 0.0],
            [0.0, 0.5, -1.0],
            6.0,
            9.81,
            0.0,
            0.05,
            256,
            WHITE,
        );
        assert!(n >= 1);
        assert_eq!(out.len(), n);
        assert_eq!(out[0].a, [0.0, 1.5, 0.0]);
        let landing = out.last().unwrap().b;
        assert!(landing[1].abs() < 1e-3, "landing near ground: {landing:?}");
        assert!(landing[2] < 0.0, "travelled forward: {landing:?}");
    }

    #[test]
    fn arc_respects_segment_cap() {
        // A flat, fast, gravity-free shot never reaches the ground: exactly
        // `segments` segments pushed.
        let mut out = Vec::new();
        let n = push_arc(
            &mut out,
            [0.0, 1.5, 0.0],
            [0.0, 0.0, -1.0],
            50.0,
            0.0,
            0.0,
            0.05,
            8,
            WHITE,
        );
        assert_eq!(n, 8);
    }

    #[test]
    fn ribbon_emits_rails_and_rungs() {
        // 3-point straight ribbon in the XZ plane (up = +Y): 2 rail segments ×
        // 2 rails + 3 rungs = 7 lines.
        let mut out = Vec::new();
        let n = push_ribbon(
            &mut out,
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]],
            0.4,
            [0.0, 1.0, 0.0],
            WHITE,
        );
        assert_eq!(n, 7);
        assert_eq!(out.len(), 7);
    }

    #[test]
    fn ribbon_rejects_degenerate_inputs() {
        let mut out = Vec::new();
        assert_eq!(
            push_ribbon(&mut out, &[[0.0; 3]], 0.4, [0.0, 1.0, 0.0], WHITE),
            0
        );
        assert_eq!(
            push_ribbon(
                &mut out,
                &[[0.0; 3], [1.0, 0.0, 0.0]],
                0.0,
                [0.0, 1.0, 0.0],
                WHITE
            ),
            0
        );
        assert_eq!(
            push_ribbon(&mut out, &[[0.0; 3], [1.0, 0.0, 0.0]], 0.4, [0.0; 3], WHITE),
            0
        );
        assert!(out.is_empty());
    }
}

mod tick_tests {
    use super::*;

    fn line_len(l: &OverlayLine) -> f32 {
        let d = [l.b[0] - l.a[0], l.b[1] - l.a[1], l.b[2] - l.a[2]];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
    }

    /// A non-degenerate box emits 3 ticks per corner × 8 corners = 24 lines.
    #[test]
    fn obb_non_degenerate_pushes_24_lines() {
        let corners = unit_cube_corners(1.0);
        let mut out = Vec::new();
        push_obb_corner_ticks(&mut out, &corners, [1.0, 1.0, 1.0, 1.0], 0.25);
        assert_eq!(out.len(), 24);
    }

    /// Every OBB tick must be no longer than half its edge — never overshooting the
    /// adjacent corner — for both a unit cube and a sub-fallback tiny cube.
    #[test]
    fn obb_ticks_never_exceed_half_edge() {
        for edge in [1.0_f32, 0.005] {
            let corners = unit_cube_corners(edge);
            let mut out = Vec::new();
            push_obb_corner_ticks(&mut out, &corners, [1.0, 1.0, 1.0, 1.0], 0.25);
            let half = edge * 0.5 + 1e-4;
            for l in &out {
                let len = line_len(l);
                assert!(len.is_finite(), "tick length finite");
                assert!(
                    len <= half,
                    "tick {len} exceeds half edge {half} (edge {edge})"
                );
            }
        }
    }

    /// AABB ticks must stay within half the span on every axis for tiny and flat
    /// (zero-span) boxes, with no NaN.
    #[test]
    fn aabb_ticks_clamped_on_tiny_and_flat_box() {
        // Tiny cube.
        let mut out = Vec::new();
        push_aabb_corner_ticks(&mut out, [0.0; 3], [0.005; 3], [1.0; 4], 0.25);
        assert_eq!(out.len(), 24);
        for l in &out {
            assert!(
                line_len(l) <= 0.005 * 0.5 + 1e-4,
                "tiny-box tick within half span"
            );
        }

        // Flat box: zero span in Z. Ticks along Z must be zero-length.
        let mut flat = Vec::new();
        push_aabb_corner_ticks(&mut flat, [0.0; 3], [2.0, 2.0, 0.0], [1.0; 4], 0.25);
        for l in &flat {
            let len = line_len(l);
            assert!(len.is_finite(), "flat-box tick finite");
            // No tick may exceed half the largest span (1.0).
            assert!(len <= 1.0 + 1e-4);
            // A tick purely along Z (a==b in X and Y) must be zero-length.
            if (l.a[0] - l.b[0]).abs() < 1e-6 && (l.a[1] - l.b[1]).abs() < 1e-6 {
                assert!(len < 1e-6, "Z tick on flat box must be zero-length");
            }
        }
    }

    /// Corners of an axis-aligned cube of the given `edge`, ordered by bit-index
    /// (bit 0 = X, bit 1 = Y, bit 2 = Z) as `push_obb_corner_ticks` expects.
    fn unit_cube_corners(edge: f32) -> [[f32; 3]; 8] {
        let mut c = [[0.0; 3]; 8];
        for (i, slot) in c.iter_mut().enumerate() {
            *slot = [
                if i & 1 == 0 { 0.0 } else { edge },
                if i & 2 == 0 { 0.0 } else { edge },
                if i & 4 == 0 { 0.0 } else { edge },
            ];
        }
        c
    }
}

#[cfg(all(test, feature = "editor-overlay"))]
mod bone_tests {
    use super::*;

    fn approx(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-4)
    }

    #[test]
    fn octahedron_pushes_twelve_lines() {
        let mut out = Vec::new();
        push_bone_octahedron(
            &mut out,
            [0.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [0.4, 0.65, 1.0, 1.0],
        );
        // 4 head→ring + 4 ring→tail + 4 ring loop.
        assert_eq!(out.len(), 12);
    }

    #[test]
    fn octahedron_spans_head_to_tail() {
        let head = [1.0, 0.0, 0.0];
        let tail = [1.0, 4.0, 0.0];
        let mut out = Vec::new();
        push_bone_octahedron(&mut out, head, tail, [0.4, 0.65, 1.0, 1.0]);
        // The head appears as an endpoint of the 4 head→ring lines; the tail as
        // an endpoint of the 4 ring→tail lines.
        assert!(
            out.iter().any(|l| approx(l.a, head)),
            "head endpoint present"
        );
        assert!(
            out.iter().any(|l| approx(l.b, tail)),
            "tail endpoint present"
        );
    }

    #[test]
    fn degenerate_bone_pushes_nothing() {
        let mut out = Vec::new();
        push_bone_octahedron(
            &mut out,
            [1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
        );
        assert!(out.is_empty());
    }

    #[test]
    fn axis_aligned_bone_stays_off_axis_ring() {
        // A bone aligned to +X must still produce a non-degenerate perpendicular
        // ring (regression guard for the reference-vector selection).
        let mut out = Vec::new();
        push_bone_octahedron(
            &mut out,
            [0.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        );
        assert_eq!(out.len(), 12);
        // No line should be zero-length.
        for l in &out {
            let d = [l.b[0] - l.a[0], l.b[1] - l.a[1], l.b[2] - l.a[2]];
            let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            assert!(len2 > 1e-6, "no degenerate edge");
        }
    }
}

#[cfg(feature = "editor-overlay")]
const AXIS_WGSL: &str = r#"
struct Frame {
    view_proj: mat4x4<f32>,
}
@group(0) @binding(0) var<uniform> frame: Frame;

struct VertIn {
    @location(0) position: vec3<f32>,
    @location(1) is_far:   f32,
    @location(2) color:    vec4<f32>,
}

struct VertOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_main(in: VertIn) -> VertOut {
    var clip_pos = frame.view_proj * vec4<f32>(in.position, 1.0);
    if in.is_far > 0.5 {
        // Pin the far endpoint to the clip-space far plane (z = w → NDC depth 1.0).
        // Screen-space XY direction is preserved; the line extends to the horizon.
        clip_pos.z = clip_pos.w;
    }
    var out: VertOut;
    out.clip_pos = clip_pos;
    out.color    = in.color;
    return out;
}

@fragment
fn fs_main(in: VertOut) -> @location(0) vec4<f32> {
    return in.color;
}
"#;
