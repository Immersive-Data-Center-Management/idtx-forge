//! Behavior-driven prim highlight rendering (U3.6): surface **tint** and
//! silhouette **outline**, both driven by the per-frame
//! [`RenderWorldSnapshot::highlight_tints`] / `highlight_outlines` channels a
//! behavior fills through the `overlay` host bucket.
//!
//! The two effects share one primitive — a per-prim **selection mask**: the
//! highlighted prims are re-rasterised into an `R8Unorm` mask, then
//!
//! * **tint** composites a flat colour over the masked pixels (alpha = strength), and
//! * **outline** feeds the mask into the jump-flood [`SilhouetteOutlinePass`] and
//!   composites an anti-aliased edge band in the behavior's colour.
//!
//! Everything here is **decoupled** from the beauty/draw-merge/`mesh_gpu_cache`
//! hot path: the mask replays a small, self-contained geometry cache keyed by
//! prim path (rebuilt only when the highlight set or a prim's geometry serial
//! changes), so a prim folded into a merged batch is still outlined correctly.
//! The passes run after tonemapping, over the LDR [`crate::COLOR_FORMAT`] target,
//! exactly like the selection [`crate::overlay::OverlayPass`].

use std::collections::HashMap;

use glam::Mat4;
use wgpu::util::DeviceExt;

use crate::postprocess::{
    group_outlines_by_color, jfa_step_sequence, ping_pong_schedule, JfaStepUniform,
    OutlineCompositeUniform, SilhouetteOutlinePass, DEFAULT_OUTLINE_SOFTNESS_PX,
    DEFAULT_OUTLINE_THICKNESS_PX, SEED_FORMAT,
};
use crate::scene_extract::RenderWorldSnapshot;
use crate::COLOR_FORMAT;

/// Single-channel selection-mask format (seeded by [`SilhouetteOutlinePass`]).
const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

/// Per-prim geometry uploaded for the mask pass — positions + indices + a model
/// uniform. Independent of the beauty-pass `mesh_gpu_cache`.
struct MaskMesh {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    index_count: u32,
    model_buf: wgpu::Buffer,
    model_bg: wgpu::BindGroup,
    /// Real USD prim path this entry was highlighted under ([`GpuMeshDesc::prim_path`]).
    /// The cache is keyed by the *outer* [`RenderMeshInstance::prim_path`] (unique
    /// per GeomSubset piece), but a highlight request names the real path, so the
    /// mask pass selects entries by this field — letting every subset piece of a
    /// multi-material mesh be masked together.
    ///
    /// [`GpuMeshDesc::prim_path`]: crate::mesh::GpuMeshDesc::prim_path
    real_path: String,
    /// `GpuMeshDesc::serial` this entry was built from; a change forces a rebuild.
    serial: u64,
    /// Render-space world transform last written into `model_buf`.
    last_model: Mat4,
    /// Marks entries still referenced this frame (stale ones are evicted).
    touched: bool,
}

/// Viewport-sized GPU targets for the mask + jump-flood ping-pong.
struct HighlightTargets {
    width: u32,
    height: u32,
    mask_view: wgpu::TextureView,
    seed_views: [wgpu::TextureView; 2],
}

/// Renders behavior-requested tint + outline highlights over the LDR target.
pub struct HighlightRenderer {
    sampler: wgpu::Sampler,
    /// Shared camera `view_proj` uniform (updated per frame).
    cam_buf: wgpu::Buffer,
    cam_bg: wgpu::BindGroup,
    model_bgl: wgpu::BindGroupLayout,
    mask_pipeline: wgpu::RenderPipeline,
    /// Flat-fill tint composite (sampler @0, mask @1, colour uniform @2).
    tint_bgl: wgpu::BindGroupLayout,
    tint_pipeline: wgpu::RenderPipeline,
    /// Edge composite over the JFA seed field (sampler @0, seed @1, mask @2,
    /// outline uniform @3). Alpha-blended in place; reads the mask so the band
    /// is drawn strictly *outside* the silhouette.
    outline_bgl: wgpu::BindGroupLayout,
    outline_pipeline: wgpu::RenderPipeline,
    /// Seed-init + jump-flood pipelines (the outline distance field). The
    /// library composite is replaced by `outline_pipeline` above.
    outline: SilhouetteOutlinePass,
    targets: Option<HighlightTargets>,
    meshes: HashMap<String, MaskMesh>,
}

impl std::fmt::Debug for HighlightRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HighlightRenderer")
            .field("cached_meshes", &self.meshes.len())
            .finish_non_exhaustive()
    }
}

impl HighlightRenderer {
    /// Build the mask + tint pipelines and the (dead-code-until-now) jump-flood
    /// outline pass. Targets and the per-prim mask geometry are allocated lazily.
    #[must_use]
    pub fn new(device: &wgpu::Device) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Highlight Sampler"),
            // NonFiltering: the mask/seed bind-group layouts use point sampling.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // Camera uniform (view_proj) at group 0.
        let cam_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Highlight Cam BGL"),
            entries: &[uniform_entry(0, wgpu::ShaderStages::VERTEX)],
        });
        let cam_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Highlight Cam Buf"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cam_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Highlight Cam BG"),
            layout: &cam_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: cam_buf.as_entire_binding(),
            }],
        });

        // Per-prim model uniform at group 1.
        let model_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Highlight Model BGL"),
            entries: &[uniform_entry(0, wgpu::ShaderStages::VERTEX)],
        });

        let mask_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Highlight Mask Shader"),
            source: wgpu::ShaderSource::Wgsl(MASK_WGSL.into()),
        });
        let mask_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Highlight Mask Layout"),
            bind_group_layouts: &[Some(&cam_bgl), Some(&model_bgl)],
            immediate_size: 0,
        });
        let mask_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Highlight Mask Pipeline"),
            layout: Some(&mask_layout),
            vertex: wgpu::VertexState {
                module: &mask_shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 12,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                        shader_location: 0,
                    }],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &mask_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: MASK_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            // Double-sided: a highlight silhouette must include back faces of
            // thin/open geometry; no depth test (the cue shows through occluders).
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // Tint composite: flat colour over masked pixels, alpha-blended in place.
        let tint_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Highlight Tint BGL"),
            entries: &[
                sampler_entry(0),
                tex_entry(1),
                uniform_entry(2, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let tint_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Highlight Tint Shader"),
            source: wgpu::ShaderSource::Wgsl(TINT_WGSL.into()),
        });
        let tint_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Highlight Tint Layout"),
            bind_group_layouts: &[Some(&tint_bgl)],
            immediate_size: 0,
        });
        let tint_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Highlight Tint Pipeline"),
            layout: Some(&tint_layout),
            vertex: wgpu::VertexState {
                module: &tint_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &tint_shader,
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

        // Outline composite: anti-aliased edge band over the JFA seed field,
        // alpha-blended in place; the mask excludes interior pixels.
        let outline_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Highlight Outline BGL"),
            entries: &[
                sampler_entry(0),
                tex_entry(1),
                tex_entry(2),
                uniform_entry(3, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let outline_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Highlight Outline Shader"),
            source: wgpu::ShaderSource::Wgsl(OUTLINE_WGSL.into()),
        });
        let outline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Highlight Outline Layout"),
            bind_group_layouts: &[Some(&outline_bgl)],
            immediate_size: 0,
        });
        let outline_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Highlight Outline Pipeline"),
            layout: Some(&outline_layout),
            vertex: wgpu::VertexState {
                module: &outline_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &outline_shader,
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
            cam_buf,
            cam_bg,
            model_bgl,
            mask_pipeline,
            tint_bgl,
            tint_pipeline,
            outline_bgl,
            outline_pipeline,
            outline: SilhouetteOutlinePass::new(device, COLOR_FORMAT),
            targets: None,
            meshes: HashMap::new(),
        }
    }

    /// Encode the tint + outline passes for this frame over `color_view`.
    /// No-op when the snapshot carries no highlights.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        snapshot: &RenderWorldSnapshot,
        color_view: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) {
        if snapshot.highlight_tints.is_empty() && snapshot.highlight_outlines.is_empty() {
            return;
        }
        let Some(camera) = snapshot.camera.as_ref() else {
            return;
        };
        if width == 0 || height == 0 {
            return;
        }

        // Camera view_proj (render-space → clip), matching the beauty pass.
        let view_proj = (camera.projection * camera.view).to_cols_array_2d();
        queue.write_buffer(&self.cam_buf, 0, bytemuck::cast_slice(&[view_proj]));

        self.ensure_targets(device, width, height);
        self.sync_meshes(device, queue, snapshot);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Highlight Encoder"),
        });

        // ── Tint — one mask + flat-fill composite per distinct colour ──────────
        if !snapshot.highlight_tints.is_empty() {
            // Group paths by colour so same-coloured prims share one mask pass.
            let mut by_color: Vec<([u32; 4], [f32; 4], Vec<&str>)> = Vec::new();
            for (path, color) in &snapshot.highlight_tints {
                let key = color.map(f32::to_bits);
                if let Some(g) = by_color.iter_mut().find(|g| g.0 == key) {
                    g.2.push(path.as_str());
                } else {
                    by_color.push((key, *color, vec![path.as_str()]));
                }
            }
            for (_, color, paths) in &by_color {
                self.render_mask(&mut encoder, &paths.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
                self.composite_tint(device, &mut encoder, color_view, *color);
            }
        }

        // ── Outline — one mask + jump-flood + composite per distinct colour ────
        if !snapshot.highlight_outlines.is_empty() {
            let groups = group_outlines_by_color(&snapshot.highlight_outlines);
            let max_dim = width.max(height);
            for (color, thickness, paths) in &groups {
                self.render_mask(&mut encoder, paths);
                self.composite_outline(device, &mut encoder, color_view, *color, *thickness, max_dim);
            }
        }

        queue.submit([encoder.finish()]);
    }

    /// (Re)allocate the mask / seed / scene-copy targets for `width × height`.
    fn ensure_targets(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if self
            .targets
            .as_ref()
            .is_some_and(|t| t.width == width && t.height == height)
        {
            return;
        }
        let mk = |label: &str, format: wgpu::TextureFormat, extra: wgpu::TextureUsages| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | extra,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let mask_view = mk("Highlight Mask", MASK_FORMAT, wgpu::TextureUsages::empty());
        let seed_views = [
            mk("Highlight Seed 0", SEED_FORMAT, wgpu::TextureUsages::empty()),
            mk("Highlight Seed 1", SEED_FORMAT, wgpu::TextureUsages::empty()),
        ];
        self.targets = Some(HighlightTargets {
            width,
            height,
            mask_view,
            seed_views,
        });
    }

    /// Build / refresh the per-prim mask geometry for every highlighted prim,
    /// then evict entries no prim referenced this frame.
    fn sync_meshes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        snapshot: &RenderWorldSnapshot,
    ) {
        for m in self.meshes.values_mut() {
            m.touched = false;
        }
        let wanted = |path: &str| {
            snapshot.highlight_tints.contains_key(path)
                || snapshot.highlight_outlines.iter().any(|(p, _, _)| p == path)
        };
        for inst in &snapshot.meshes {
            // Match by the real USD path (inner `GpuMeshDesc::prim_path`), not the
            // synthetic outer `prim_path` of a GeomSubset piece — see
            // [`instance_real_path`].
            if !wanted(instance_real_path(inst)) {
                continue;
            }
            let model = inst.world_matrix;
            // Cache key is the outer `prim_path`: unique per subset piece, so the
            // several pieces of one multi-material mesh each get their own entry.
            let needs_build = match self.meshes.get_mut(&inst.prim_path) {
                Some(m) if m.serial == inst.mesh.serial => {
                    m.touched = true;
                    if m.last_model != model {
                        queue.write_buffer(
                            &m.model_buf,
                            0,
                            bytemuck::cast_slice(&[model.to_cols_array_2d()]),
                        );
                        m.last_model = model;
                    }
                    false
                }
                _ => true,
            };
            if needs_build {
                let built = self.build_mask_mesh(
                    device,
                    instance_real_path(inst),
                    inst.mesh.as_ref(),
                    model,
                );
                self.meshes.insert(inst.prim_path.clone(), built);
            }
        }
        self.meshes.retain(|_, m| m.touched);
    }

    fn build_mask_mesh(
        &self,
        device: &wgpu::Device,
        real_path: &str,
        mesh: &crate::mesh::GpuMeshDesc,
        model: Mat4,
    ) -> MaskMesh {
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Highlight Mask VBuf"),
            contents: bytemuck::cast_slice(&mesh.positions),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Highlight Mask IBuf"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let model_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Highlight Model Buf"),
            contents: bytemuck::cast_slice(&[model.to_cols_array_2d()]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let model_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Highlight Model BG"),
            layout: &self.model_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: model_buf.as_entire_binding(),
            }],
        });
        MaskMesh {
            vbuf,
            ibuf,
            index_count: u32::try_from(mesh.indices.len()).unwrap_or(0),
            model_buf,
            model_bg,
            real_path: real_path.to_owned(),
            serial: mesh.serial,
            last_model: model,
            touched: true,
        }
    }

    /// Clear the mask and rasterise every prim whose real USD path is in `paths`
    /// into it (value 1.0). `paths` holds the real highlight paths for one colour
    /// group; cache entries are selected by [`MaskMesh::real_path`], so all
    /// GeomSubset pieces of a multi-material mesh are drawn together.
    fn render_mask(&self, encoder: &mut wgpu::CommandEncoder, paths: &[String]) {
        let wanted: std::collections::HashSet<&str> =
            paths.iter().map(String::as_str).collect();
        let targets = self.targets.as_ref().expect("targets allocated");
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Highlight Mask Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &targets.mask_view,
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
        pass.set_pipeline(&self.mask_pipeline);
        pass.set_bind_group(0, &self.cam_bg, &[]);
        for m in self.meshes.values() {
            if !wanted.contains(m.real_path.as_str()) {
                continue;
            }
            if m.index_count == 0 {
                continue;
            }
            pass.set_bind_group(1, &m.model_bg, &[]);
            pass.set_vertex_buffer(0, m.vbuf.slice(..));
            pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..m.index_count, 0, 0..1);
        }
    }

    /// Composite a flat tint over the current mask, alpha-blended into `color_view`.
    fn composite_tint(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        color_view: &wgpu::TextureView,
        color: [f32; 4],
    ) {
        let targets = self.targets.as_ref().expect("targets allocated");
        let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Highlight Tint Uniform"),
            contents: bytemuck::cast_slice(&[color]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Highlight Tint BG"),
            layout: &self.tint_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&targets.mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ubuf.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Highlight Tint Composite"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.tint_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Seed the current mask, run the jump-flood propagation, and composite an
    /// anti-aliased edge band in `color` alpha-blended into `color_view`. The
    /// composite reads the mask so the band is drawn strictly outside the
    /// silhouette (interior pixels are left untouched).
    #[allow(clippy::cast_precision_loss)] // step/texel are small viewport pixel counts
    fn composite_outline(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        color_view: &wgpu::TextureView,
        color: [f32; 4],
        thickness_px: f32,
        max_dim: u32,
    ) {
        let targets = self.targets.as_ref().expect("targets allocated");

        // Seed-init: mask → seed field in seed[0].
        {
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Highlight Seed BG"),
                layout: self.outline.seed_bind_group_layout(),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&targets.mask_view),
                    },
                ],
            });
            let mut pass = begin_fullscreen(encoder, "Highlight Seed", &targets.seed_views[0]);
            pass.set_pipeline(&self.outline.seed_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }

        // Jump-flood ping-pong: seed[k%2] → seed[(k+1)%2].
        let steps = jfa_step_sequence(max_dim);
        let schedule = ping_pong_schedule(steps.len(), 0);
        let texel = [1.0 / targets.width as f32, 1.0 / targets.height as f32];
        for (hop, step_px) in schedule.iter().zip(&steps) {
            let step_u = JfaStepUniform {
                step_px: *step_px as f32,
                _pad: 0.0,
                texel_size: texel,
            };
            let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Highlight JFA Step"),
                contents: bytemuck::bytes_of(&step_u),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Highlight JFA BG"),
                layout: self.outline.jfa_bind_group_layout(),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&targets.seed_views[hop.read]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: ubuf.as_entire_binding(),
                    },
                ],
            });
            let mut pass =
                begin_fullscreen(encoder, "Highlight JFA", &targets.seed_views[hop.write]);
            pass.set_pipeline(&self.outline.jfa_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
        let final_seed = crate::postprocess::final_target(steps.len(), 0);

        // Composite: seed field + mask → anti-aliased edge band, alpha-blended.
        // thickness_px == 0 falls back to the default; softness scales with it so
        // thin outlines stay crisp and thick ones keep a proportional soft edge.
        let thickness = if thickness_px > 0.0 {
            thickness_px
        } else {
            DEFAULT_OUTLINE_THICKNESS_PX
        };
        let softness = DEFAULT_OUTLINE_SOFTNESS_PX.min(thickness * 0.5);
        let ucomp = OutlineCompositeUniform {
            color,
            thickness_px: thickness,
            softness_px: softness,
            _pad: [0.0, 0.0],
        };
        let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Highlight Outline Uniform"),
            contents: bytemuck::bytes_of(&ucomp),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Highlight Outline Composite BG"),
            layout: &self.outline_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&targets.seed_views[final_seed]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&targets.mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: ubuf.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Highlight Outline Composite"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.outline_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }
}

fn begin_fullscreen<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    label: &str,
    view: &'a wgpu::TextureView,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
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
    })
}

/// The **real USD prim path** a mask entry must be matched against.
///
/// A multi-material mesh is split per `GeomSubset` by the extractor into several
/// [`RenderMeshInstance`]s whose *outer* [`RenderMeshInstance::prim_path`] is a
/// synthetic `…/subset_<mat>` / `…/remainder` label — not a real USD path. The
/// true prim path survives verbatim on the *inner* [`GpuMeshDesc::prim_path`]
/// ([`crate::mesh::GpuMeshDesc::subset_by_faces`] clones it), which is exactly
/// what a behavior highlights by (`resolve_overlay_highlights` emits the gprim's
/// real path). Matching here on the inner path — as the transform-patch and
/// skinning paths in `scene_extract` already do — is what lets an outlined
/// multi-material mesh light up at all.
///
/// [`GpuMeshDesc::prim_path`]: crate::mesh::GpuMeshDesc::prim_path
fn instance_real_path(inst: &crate::scene_extract::RenderMeshInstance) -> &str {
    &inst.mesh.prim_path
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
        count: None,
    }
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// Mask vertex: project local position through model + camera; fragment writes
/// a constant 1.0 into the `R8Unorm` mask.
const MASK_WGSL: &str = r"
struct Cam { view_proj: mat4x4<f32> }
struct Model { model: mat4x4<f32> }
@group(0) @binding(0) var<uniform> cam: Cam;
@group(1) @binding(0) var<uniform> m: Model;

@vertex
fn vs_main(@location(0) pos: vec3<f32>) -> @builtin(position) vec4<f32> {
    return cam.view_proj * m.model * vec4<f32>(pos, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 0.0, 0.0, 1.0);
}
";

/// Tint composite: fullscreen triangle; flat colour where the mask is set,
/// alpha = the colour's alpha (strength). Alpha-blended over the scene.
const TINT_WGSL: &str = r"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0)       uv:  vec2<f32>,
}
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = select(-1.0, 3.0, vi == 1u);
    let y = select(-1.0, 3.0, vi == 2u);
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

struct TintU { color: vec4<f32> }
@group(0) @binding(0) var samp: sampler;
@group(0) @binding(1) var mask: texture_2d<f32>;
@group(0) @binding(2) var<uniform> u: TintU;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let m = textureSample(mask, samp, in.uv).r;
    let a = u.color.a * select(0.0, 1.0, m > 0.5);
    return vec4<f32>(u.color.rgb, a);
}
";

/// Outline composite: distance from each exterior pixel to the nearest seed
/// (silhouette) → an anti-aliased edge band in `u.color`, alpha-blended over the
/// scene. Reads the mask to leave interior pixels untouched. Mirrors
/// [`crate::postprocess::outline_alpha`].
const OUTLINE_WGSL: &str = r"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0)       uv:  vec2<f32>,
}
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = select(-1.0, 3.0, vi == 1u);
    let y = select(-1.0, 3.0, vi == 2u);
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

struct OutlineU {
    color:        vec4<f32>,
    thickness_px: f32,
    softness_px:  f32,
    _pad:         vec2<f32>,
}
@group(0) @binding(0) var samp: sampler;
@group(0) @binding(1) var seed: texture_2d<f32>;
@group(0) @binding(2) var mask: texture_2d<f32>;
@group(0) @binding(3) var<uniform> u: OutlineU;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Interior pixels (inside the silhouette) are never outlined.
    if (textureSample(mask, samp, in.uv).r > 0.5) {
        return vec4<f32>(0.0);
    }
    let dims = vec2<f32>(textureDimensions(seed));
    let here = in.uv * dims;
    let s = textureSample(seed, samp, in.uv).xy;
    if (s.x < 0.0) {
        return vec4<f32>(0.0);          // no seed reached this pixel
    }
    let dist = distance(here, s);
    let inner = max(u.thickness_px - max(u.softness_px, 0.0), 0.0);
    var a = 0.0;
    if (dist <= inner) {
        a = 1.0;
    } else if (dist < u.thickness_px) {
        a = 1.0 - (dist - inner) / (u.thickness_px - inner);
    }
    return vec4<f32>(u.color.rgb, u.color.a * a);
}
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::fallback_material;
    use crate::mesh::tessellate_cube;
    use crate::scene_extract::RenderMeshInstance;
    use glam::Mat4;
    use idtx_usd::RenderPurpose;
    use std::sync::Arc;

    /// Outer cache keys ([`RenderMeshInstance::prim_path`], unique per subset
    /// piece) of every mesh whose *real* USD path satisfies `wanted` — the exact
    /// selection [`HighlightRenderer::sync_meshes`] / [`HighlightRenderer::render_mask`]
    /// perform, lifted into a pure form so the GeomSubset-split case is testable
    /// without a GPU. Mirrors the live code by deciding membership through
    /// [`instance_real_path`].
    fn wanted_mask_keys<'a>(
        meshes: &'a [RenderMeshInstance],
        wanted: impl Fn(&str) -> bool,
    ) -> Vec<&'a str> {
        meshes
            .iter()
            .filter(|inst| wanted(instance_real_path(inst)))
            .map(|inst| inst.prim_path.as_str())
            .collect()
    }

    /// Build a [`RenderMeshInstance`] with a distinct *outer* `prim_path` and
    /// *inner* [`GpuMeshDesc::prim_path`] (the real USD path), mirroring how the
    /// extractor emits GeomSubset pieces.
    fn instance(outer: &str, real: &str) -> RenderMeshInstance {
        let mut mesh = tessellate_cube(0.5);
        mesh.prim_path = real.to_owned();
        RenderMeshInstance {
            prim_path: outer.to_owned(),
            mesh: Arc::new(mesh),
            world_matrix: Mat4::IDENTITY,
            material: fallback_material(real),
            purpose: RenderPurpose::Default,
            casts_shadow: true,
        }
    }

    /// A GeomSubset-split multi-material mesh (as in `2S_MM_lowpoly.usdz`: one USD
    /// `Mesh` with three `materialBind` subsets) is masked when a behavior
    /// outlines it by its real prim path — every subset piece must be selected,
    /// so the whole asset lights up.
    #[test]
    fn multi_material_subset_mesh_is_masked_by_real_path() {
        let rack = "/World/Rack";
        let meshes = vec![
            // Three per-subset instances + remainder, all carrying the real path
            // on the inner GpuMeshDesc but a synthetic outer prim_path.
            instance("/World/Rack/subset_white", rack),
            instance("/World/Rack/subset_grey", rack),
            instance("/World/Rack/remainder", rack),
            // A single-material implicit gprim: outer == inner == real path.
            instance("/World/Box", "/World/Box"),
        ];

        // The behavior outlined the rack by its real prim path (exactly what
        // `resolve_overlay_highlights` emits after collision-body → leaf-gprim
        // expansion).
        let wanted = |p: &str| p == rack;

        let keys = wanted_mask_keys(&meshes, wanted);
        assert_eq!(
            keys,
            vec![
                "/World/Rack/subset_white",
                "/World/Rack/subset_grey",
                "/World/Rack/remainder",
            ],
            "all subset pieces of the outlined mesh must be masked"
        );

        // Regression guard: the previous code matched on the OUTER prim_path,
        // which for a subset-split mesh never equals the real highlighted path —
        // so nothing was masked and the outline silently disappeared.
        let outer_match: Vec<&str> = meshes
            .iter()
            .filter(|i| wanted(&i.prim_path))
            .map(|i| i.prim_path.as_str())
            .collect();
        assert!(
            outer_match.is_empty(),
            "outer-path matching misses every subset piece — the bug this guards"
        );
    }

    /// A single-material mesh / implicit gprim (outer path == real path) is still
    /// selected — the fix must not regress the common path.
    #[test]
    fn single_material_mesh_matches_on_its_own_path() {
        let meshes = vec![instance("/World/Box", "/World/Box")];
        let keys = wanted_mask_keys(&meshes, |p| p == "/World/Box");
        assert_eq!(keys, vec!["/World/Box"]);
    }
}

