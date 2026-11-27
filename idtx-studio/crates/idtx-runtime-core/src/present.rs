//! Presents the session's offscreen render to the window swapchain.
//!
//! The editor composites `StudioSession::display_view()` into an egui image
//! widget; the headless/windowed runtime owns the whole surface, so it blits
//! that offscreen colour view onto the acquired swapchain image with a
//! fullscreen-triangle pass.
//!
//! The blit pipeline is built for the *surface* format (typically
//! `Bgra8UnormSrgb`), not the renderer's internal `Rgba8UnormSrgb`, so this
//! reuses [`idtx_render::pipeline::build_blit_pipeline`] rather than the
//! renderer's own blit pipeline. Sampling an sRGB source and writing an sRGB
//! target is a passthrough, so no colour-space fixup is needed.
//!
//! The bind group is cached and rebuilt only when the source view changes
//! (i.e. on resize, when the session recreates its frame target) — never
//! per frame.

use idtx_render::pipeline::{BlitPipeline, build_blit_pipeline};

/// Owns the surface-format blit pipeline and a cached source bind group.
pub struct Presenter {
    pipeline: BlitPipeline,
    /// Cached (source-view-generation, bind group). Rebuilt when the source
    /// view identity changes.
    bind_group: Option<wgpu::BindGroup>,
    /// Generation counter the caller bumps whenever the source view is
    /// recreated (on resize), invalidating the cached bind group.
    src_generation: u64,
    cached_generation: u64,
}

impl std::fmt::Debug for Presenter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Presenter")
            .field("src_generation", &self.src_generation)
            .field("cached_generation", &self.cached_generation)
            .field("bind_group_cached", &self.bind_group.is_some())
            .finish()
    }
}

impl Presenter {
    /// Build a presenter targeting `surface_format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        Self {
            pipeline: build_blit_pipeline(device, surface_format),
            bind_group: None,
            src_generation: 0,
            cached_generation: u64::MAX,
        }
    }

    /// Signal that the source colour view was recreated (e.g. viewport resize),
    /// so the cached bind group is rebuilt on the next [`Self::present`].
    pub fn invalidate(&mut self) {
        self.src_generation = self.src_generation.wrapping_add(1);
    }

    /// Blit `src` (the session's offscreen colour view) onto `dst` (the
    /// acquired swapchain image view). Reuses the cached bind group unless the
    /// source was invalidated since the last call.
    pub fn present(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        src: &wgpu::TextureView,
        dst: &wgpu::TextureView,
    ) {
        if self.bind_group.is_none() || self.cached_generation != self.src_generation {
            self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("runtime-present-bg"),
                layout: &self.pipeline.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Sampler(&self.pipeline.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(src),
                    },
                ],
            }));
            self.cached_generation = self.src_generation;
        }
        let bind_group = self.bind_group.as_ref().expect("bind group just set");

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("runtime-present-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("runtime-present-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst,
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
            pass.set_pipeline(&self.pipeline.pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
    }
}
