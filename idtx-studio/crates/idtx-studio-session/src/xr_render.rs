//! Stage C GPU resources + per-eye rendering for the real-device `OpenXR` path.
//!
//! Wraps the runtime's swapchain images as wgpu textures (via wgpu-hal — D3D12
//! `ID3D12Resource*` on Windows PCVR, `VkImage` on Quest/Android), renders the
//! scene once per eye into an offscreen [`FrameTarget`] using an off-axis
//! projection built from the eye's asymmetric FOV, and copies each result into
//! the acquired swapchain image for compositor submission.
//!
//! The eye-target allocation, per-eye render loop, depth resolve/copy, camera
//! math, and desktop mirror are graphics-API-agnostic; only the swapchain-image
//! wrapping (`wrap_*` / `wrap_*_vk`) and the matching `XrGpu::new` constructor
//! differ per backend. Compiled whenever a real graphics session is (`idtx_xr`).
//! Not unit-testable (needs a headset).

#[cfg(windows)]
use std::ffi::c_void;

use glam::{Mat4, Quat, Vec3, Vec4};
use idtx_openxr_sys::{XrEye, XrFrame, XrGraphicsSession};
use idtx_render::{FrameTarget, RenderCameraDesc, RenderWorldSnapshot, Renderer};

use crate::xr_preview::XrPreviewError;

/// Per-eye render timings returned by [`XrGpu::render_eyes`].
///
/// With `metrics` enabled this carries the two [`RenderFrameTimings`] the
/// renderer produced (one per eye) plus the per-eye CPU cost of the
/// offscreen→swapchain color blit and the depth resolve+copy — both untimed by
/// the renderer itself (see docs/xr-quest-performance-analysis.md §4 Gap C).
/// Without the feature the type is zero-size so the call site pays nothing.
#[cfg(feature = "metrics")]
#[derive(Debug, Clone, Default)]
pub(crate) struct EyeTimings {
    /// Renderer frame timings, one per eye.
    pub frames: [idtx_render::metrics::RenderFrameTimings; 2],
    /// Per-eye CPU time for the full-res `copy_color` blit into the color
    /// swapchain image.
    pub swapchain_copy_ns: [u64; 2],
    /// Per-eye CPU time for the depth MSAA-resolve + copy into the depth
    /// swapchain image; `0` when no depth layer is active.
    pub depth_copy_ns: [u64; 2],
}

#[cfg(not(feature = "metrics"))]
#[derive(Debug, Clone, Default)]
pub(crate) struct EyeTimings;

/// GPU resources for stereo headset rendering: one offscreen render target per
/// eye plus the wgpu textures wrapping each `OpenXR` swapchain image (cached for
/// the swapchain's lifetime — never re-wrapped per frame).
pub(crate) struct XrGpu {
    eye_targets: [FrameTarget; 2],
    eye_images: [Vec<wgpu::Texture>; 2],
    /// Depth resources for the `XR_KHR_composition_layer_depth` layer, present
    /// only when the runtime advertised the extension. `None` = color-only
    /// submission (no smear fix).
    depth: Option<XrDepth>,
    width: u32,
    height: u32,
}

/// Per-eye depth resources: the single-sample resolve target the MSAA depth is
/// resolved into, and the wgpu textures wrapping each `OpenXR` depth swapchain
/// image (the resolve is copied into the acquired one each frame).
struct XrDepth {
    /// Single-sample `Depth32Float` resolve target per eye
    /// (`RENDER_ATTACHMENT | COPY_SRC`), written by [`Renderer::resolve_depth`].
    resolve: [(wgpu::Texture, wgpu::TextureView); 2],
    /// Wrapped `OpenXR` depth swapchain images per eye (`COPY_DST`), the copy
    /// destination for the resolved depth.
    images: [Vec<wgpu::Texture>; 2],
}

impl std::fmt::Debug for XrGpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XrGpu")
            .field("extent", &(self.width, self.height))
            .field("depth_layer", &self.depth.is_some())
            .finish_non_exhaustive()
    }
}

impl XrGpu {
    /// Allocate per-eye targets and wrap the swapchain images (D3D12). Enumerates
    /// the swapchain images exactly once.
    #[cfg(windows)]
    pub(crate) fn new(renderer: &Renderer, xr: &XrGraphicsSession) -> Result<Self, XrPreviewError> {
        let (width, height) = xr.swapchain_extent();
        let ptrs = xr.swapchain_image_ptrs()?;
        let eye_images = [
            wrap_eye(&renderer.device, &ptrs[0], width, height),
            wrap_eye(&renderer.device, &ptrs[1], width, height),
        ];
        let depth_images = match xr.depth_swapchain_image_ptrs()? {
            Some(depth_ptrs) => Some([
                wrap_eye_depth(&renderer.device, &depth_ptrs[0], width, height),
                wrap_eye_depth(&renderer.device, &depth_ptrs[1], width, height),
            ]),
            None => None,
        };
        Ok(Self::assemble(
            renderer,
            width,
            height,
            eye_images,
            depth_images,
        ))
    }

    /// Allocate per-eye targets and wrap the swapchain images (Vulkan/Quest).
    /// Enumerates the swapchain images exactly once.
    #[cfg(target_os = "android")]
    pub(crate) fn new(renderer: &Renderer, xr: &XrGraphicsSession) -> Result<Self, XrPreviewError> {
        let (width, height) = xr.swapchain_extent();
        let handles = xr.swapchain_image_handles()?;
        let eye_images = [
            wrap_eye_vk(&renderer.device, &handles[0], width, height),
            wrap_eye_vk(&renderer.device, &handles[1], width, height),
        ];
        let depth_images = match xr.depth_swapchain_image_handles()? {
            Some(depth_handles) => Some([
                wrap_eye_depth_vk(&renderer.device, &depth_handles[0], width, height),
                wrap_eye_depth_vk(&renderer.device, &depth_handles[1], width, height),
            ]),
            None => None,
        };
        Ok(Self::assemble(
            renderer,
            width,
            height,
            eye_images,
            depth_images,
        ))
    }

    /// Graphics-API-agnostic finish step: build the per-eye offscreen targets
    /// (depth-capable when depth images were wrapped) and the depth resolve
    /// targets. Shared by both backend [`Self::new`] constructors.
    fn assemble(
        renderer: &Renderer,
        width: u32,
        height: u32,
        eye_images: [Vec<wgpu::Texture>; 2],
        depth_images: Option<[Vec<wgpu::Texture>; 2]>,
    ) -> Self {
        let depth = depth_images.map(|images| {
            let resolve = [
                make_depth_resolve_target(&renderer.device, width, height),
                make_depth_resolve_target(&renderer.device, width, height),
            ];
            XrDepth { resolve, images }
        });
        let eye_targets = if depth.is_some() {
            [
                renderer.create_xr_eye_target(width, height),
                renderer.create_xr_eye_target(width, height),
            ]
        } else {
            [
                renderer.create_frame_target(width, height),
                renderer.create_frame_target(width, height),
            ]
        };
        Self {
            eye_targets,
            eye_images,
            depth,
            width,
            height,
        }
    }

    /// Render both eyes for `frame` into the swapchain images.
    ///
    /// The desktop mirror reuses one eye's offscreen render (see
    /// [`Self::mirror_source`]) rather than re-rendering the scene, so the
    /// preview costs two scene renders per frame instead of three.
    ///
    /// When a depth layer is active the MSAA depth is resolved to a
    /// single-sample target and copied into the acquired depth swapchain image
    /// so [`XrGraphicsSession::submit_frame`] can chain it for reprojection.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_eyes(
        &self,
        renderer: &mut Renderer,
        frame: &XrFrame,
        snapshot: &mut RenderWorldSnapshot,
        stage_to_world: Mat4,
        recenter: Vec3,
        extra_lines: &[idtx_render::OverlayLine],
        wireframe: bool,
        near: f32,
        far: f32,
    ) -> EyeTimings {
        #[cfg(feature = "metrics")]
        let mut timings = EyeTimings::default();
        #[cfg(not(feature = "metrics"))]
        let timings = EyeTimings::default();
        for (eye_idx, eye) in frame.eyes.iter().enumerate() {
            let cam = eye_camera(eye, stage_to_world, recenter, near, far);
            snapshot.camera = Some(cam);
            // Render the scene *with* the behavior-driven overlay composites
            // (tint + silhouette outline, screen-space HUD) into the eye target —
            // exactly like the desktop path's `render_with_overlay`. A plain
            // `render` here would skip `highlight.encode`/`hud.encode`, so
            // behavior highlights never reached the headset even though the
            // snapshot carried them. `extra_lines` carries behavior-drawn overlay
            // geometry (the teleport pointer arc + landing marker); the line pass
            // early-outs when it is empty. No editor selection OBB in XR (`None`).
            #[cfg(feature = "metrics")]
            renderer.set_metrics_view(eye_idx);
            renderer.render_with_overlay(
                &self.eye_targets[eye_idx],
                snapshot,
                None,
                extra_lines,
                wireframe,
            );
            #[cfg(feature = "metrics")]
            {
                timings.frames[eye_idx] = renderer.last_frame_timings().clone();
            }
            let image_idx = eye.image_index as usize;
            #[cfg(feature = "metrics")]
            let t_copy = std::time::Instant::now();
            copy_color(
                &renderer.device,
                &renderer.queue,
                &self.eye_targets[eye_idx].color_texture,
                &self.eye_images[eye_idx][image_idx],
                self.width,
                self.height,
            );
            #[cfg(feature = "metrics")]
            {
                timings.swapchain_copy_ns[eye_idx] = t_copy.elapsed().as_nanos() as u64;
            }
            // Depth: resolve the MSAA depth to single-sample, then copy it into
            // the acquired depth swapchain image for the composition layer.
            if let Some(depth) = self.depth.as_ref() {
                #[cfg(feature = "metrics")]
                let t_depth = std::time::Instant::now();
                let depth_idx = eye.depth_image_index as usize;
                let mut encoder =
                    renderer
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("xr-eye-depth"),
                        });
                renderer.resolve_depth(
                    &mut encoder,
                    &self.eye_targets[eye_idx].depth_view,
                    &depth.resolve[eye_idx].1,
                );
                encoder.copy_texture_to_texture(
                    depth.resolve[eye_idx].0.as_image_copy(),
                    depth.images[eye_idx][depth_idx].as_image_copy(),
                    wgpu::Extent3d {
                        width: self.width,
                        height: self.height,
                        depth_or_array_layers: 1,
                    },
                );
                renderer.queue.submit(Some(encoder.finish()));
                #[cfg(feature = "metrics")]
                {
                    timings.depth_copy_ns[eye_idx] = t_depth.elapsed().as_nanos() as u64;
                }
            }
        }
        timings
    }

    /// The color texture view of one eye's offscreen render target, for
    /// mirroring into the desktop preview. `eye` is clamped to a valid index
    /// (0 = left, 1 = right).
    pub(crate) fn mirror_source(&self, eye: usize) -> &wgpu::TextureView {
        &self.eye_targets[eye.min(1)].color_view
    }

    /// Whether a depth composition layer is active (depth swapchains present and
    /// per-eye depth resolved+copied). When `true`, `render_xr` submits with a
    /// depth range so `submit_frame` chains the depth layer.
    pub(crate) fn has_depth_layer(&self) -> bool {
        self.depth.is_some()
    }
}

/// Wrap every color swapchain image pointer for one eye as a wgpu texture (D3D12).
#[cfg(windows)]
fn wrap_eye(device: &wgpu::Device, ptrs: &[*mut c_void], w: u32, h: u32) -> Vec<wgpu::Texture> {
    ptrs.iter()
        .map(|&p| {
            wrap_one(
                device,
                p,
                w,
                h,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT,
                "xr-swapchain-image",
            )
        })
        .collect()
}

/// Wrap every depth swapchain image pointer for one eye as a wgpu texture (D3D12).
/// The resolved single-sample depth is copied into these (`COPY_DST`).
#[cfg(windows)]
fn wrap_eye_depth(
    device: &wgpu::Device,
    ptrs: &[*mut c_void],
    w: u32,
    h: u32,
) -> Vec<wgpu::Texture> {
    ptrs.iter()
        .map(|&p| {
            wrap_one(
                device,
                p,
                w,
                h,
                wgpu::TextureFormat::Depth32Float,
                wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT,
                "xr-depth-swapchain-image",
            )
        })
        .collect()
}

/// Wrap every color swapchain image handle for one eye as a wgpu texture (Vulkan).
#[cfg(target_os = "android")]
fn wrap_eye_vk(device: &wgpu::Device, images: &[u64], w: u32, h: u32) -> Vec<wgpu::Texture> {
    images
        .iter()
        .map(|&img| {
            wrap_one_vk(
                device,
                img,
                w,
                h,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                wgpu::TextureUses::COPY_DST | wgpu::TextureUses::COLOR_TARGET,
                wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT,
                "xr-swapchain-image",
            )
        })
        .collect()
}

/// Wrap every depth swapchain image handle for one eye as a wgpu texture (Vulkan).
#[cfg(target_os = "android")]
fn wrap_eye_depth_vk(device: &wgpu::Device, images: &[u64], w: u32, h: u32) -> Vec<wgpu::Texture> {
    images
        .iter()
        .map(|&img| {
            wrap_one_vk(
                device,
                img,
                w,
                h,
                wgpu::TextureFormat::Depth32Float,
                wgpu::TextureUses::COPY_DST | wgpu::TextureUses::DEPTH_STENCIL_WRITE,
                wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT,
                "xr-depth-swapchain-image",
            )
        })
        .collect()
}

/// Allocate a single-sample `Depth32Float` resolve target for one eye
/// (`RENDER_ATTACHMENT | COPY_SRC`): [`Renderer::resolve_depth`] writes it, then
/// it is copied into the depth swapchain image.
fn make_depth_resolve_target(
    device: &wgpu::Device,
    w: u32,
    h: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("xr-depth-resolve"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    (tex, view)
}

/// Wrap a single `OpenXR` D3D12 swapchain image (`ID3D12Resource*`) as a wgpu
/// texture with the given `format`/`usage`.
///
/// The resource is AddRef'd (borrow + clone) so that when the resulting
/// `wgpu::Texture` drops and Releases it, `OpenXR`'s own reference is preserved.
#[cfg(windows)]
fn wrap_one(
    device: &wgpu::Device,
    ptr: *mut c_void,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
    label: &str,
) -> wgpu::Texture {
    use windows::Win32::Graphics::Direct3D12::ID3D12Resource;
    use windows::core::Interface as _;

    // SAFETY: `ptr` is a live `ID3D12Resource*` from the OpenXR swapchain; we
    // borrow it and clone (AddRef) to hand an owned reference to wgpu.
    let resource: ID3D12Resource = unsafe { ID3D12Resource::from_raw_borrowed(&ptr) }
        .expect("swapchain image pointer is non-null")
        .clone();
    let size = wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    };
    // SAFETY: `resource` is a valid single-sample 2D texture matching the
    // descriptor; the swapchain was created with a compatible `format`.
    let hal_tex = unsafe {
        wgpu::hal::dx12::Device::texture_from_raw(
            resource,
            format,
            wgpu::TextureDimension::D2,
            size,
            1,
            1,
        )
    };
    // SAFETY: `hal_tex` was created from this device's DX12 backend per `desc`.
    unsafe {
        device.create_texture_from_hal::<wgpu::hal::api::Dx12>(
            hal_tex,
            &wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            },
        )
    }
}

/// Wrap a single `OpenXR` Vulkan swapchain image (`VkImage`) as a wgpu texture.
///
/// `hal_usage` is the wgpu-hal usage recorded for barrier tracking; `usage` is
/// the public wgpu usage. The drop callback is a no-op and the memory is
/// [`External`](wgpu::hal::vulkan::TextureMemory::External) so wgpu never calls
/// `vkDestroyImage` on (nor frees memory for) the runtime-owned image — the
/// Vulkan analog of the D3D12 AddRef-via-clone in [`wrap_one`].
#[cfg(target_os = "android")]
#[allow(clippy::too_many_arguments)]
fn wrap_one_vk(
    device: &wgpu::Device,
    vk_image: u64,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
    hal_usage: wgpu::TextureUses,
    usage: wgpu::TextureUsages,
    label: &str,
) -> wgpu::Texture {
    use ash::vk::Handle as _;

    let size = wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    };
    // SAFETY: `vk_image` is a live `VkImage` owned by the OpenXR swapchain. The
    // `Some(no-op)` drop callback makes wgpu-hal skip `vkDestroyImage`, and
    // `TextureMemory::External` means wgpu owns no backing allocation to free —
    // so the runtime keeps full ownership of the image.
    let hal_tex = unsafe {
        let hal_device = device
            .as_hal::<wgpu::hal::api::Vulkan>()
            .expect("wgpu device is not on the Vulkan backend");
        hal_device.texture_from_raw(
            ash::vk::Image::from_raw(vk_image),
            &wgpu::hal::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: hal_usage,
                memory_flags: wgpu::hal::MemoryFlags::empty(),
                view_formats: vec![],
            },
            Some(Box::new(|| {})),
            wgpu::hal::vulkan::TextureMemory::External,
        )
    };
    // SAFETY: `hal_tex` was created from this device's Vulkan backend per `desc`.
    unsafe {
        device.create_texture_from_hal::<wgpu::hal::api::Vulkan>(
            hal_tex,
            &wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            },
        )
    }
}

/// Copy the resolved eye render target into the swapchain image (straight
/// sRGB→sRGB blit; both are `Rgba8UnormSrgb`, single-sample).
fn copy_color(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    src: &wgpu::Texture,
    dst: &wgpu::Texture,
    w: u32,
    h: u32,
) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("xr-eye-copy"),
    });
    encoder.copy_texture_to_texture(
        src.as_image_copy(),
        dst.as_image_copy(),
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
}

/// Build a render camera for one eye from its located pose and asymmetric FOV.
///
/// `stage_to_world` places the `OpenXR` play space (STAGE/room space, where the
/// eye pose is located) into the virtual world — the player pawn's world matrix
/// (`projection_basis * /World/Player`), carrying locomotion + snap-turn yaw. It
/// is composed *outside* the located eye pose so physical head motion and
/// head-look (carried in the eye pose itself) stay live every frame.
///
/// `recenter` is the play-space recenter offset (stage-space ground vector)
/// subtracted from the eye position before composition. The pawn root carries
/// `+recenter` (baked by `xr_player_move`), so subtracting it here cancels the
/// recenter for the view (`M · T(−s) · raw_eye = M0 · raw_eye`): the headset
/// view is invariant to a recenter snap while the static capsule — which gets no
/// such subtraction — relocates under the headset.
fn eye_camera(eye: &XrEye, stage_to_world: Mat4, recenter: Vec3, near: f32, far: f32) -> RenderCameraDesc {
    let pos = Vec3::new(
        eye.pose.position[0],
        eye.pose.position[1],
        eye.pose.position[2],
    ) - recenter;
    let rot = Quat::from_xyzw(
        eye.pose.orientation[0],
        eye.pose.orientation[1],
        eye.pose.orientation[2],
        eye.pose.orientation[3],
    );
    let view = eye_view_matrix(stage_to_world, Mat4::from_rotation_translation(rot, pos));
    let [l, r, u, d] = eye.fov;
    let projection = perspective_off_center(l.tan(), r.tan(), u.tan(), d.tan(), near, far);
    RenderCameraDesc {
        prim_path: "xr:eye".to_owned(),
        view,
        projection,
        clip_near: near,
        clip_far: far,
        fov_horizontal: (r - l).abs(),
        exposure_ev: 0.0,
    }
}

/// The view matrix for an eye whose located world (eye-to-world) transform is
/// `eye_pose_world` (STAGE space), placed into the virtual world by
/// `stage_to_world`.
///
/// `view = (stage_to_world · eye_pose_world).inverse()`. Left-multiplying before
/// the inverse keeps the eye's own rotation/translation (physical head motion +
/// head-look) as the inner transform while locomotion + snap-turn ride in
/// `stage_to_world`. An identity `stage_to_world` reproduces the raw located
/// view (the pre-pawn behaviour). Pure so it is unit-testable without a headset.
fn eye_view_matrix(stage_to_world: Mat4, eye_pose_world: Mat4) -> Mat4 {
    (stage_to_world * eye_pose_world).inverse()
}


/// Right-handed, `z ∈ [0, 1]` (wgpu/D3D) off-center perspective from the four
/// FOV tangents. Matches the depth convention of `Mat4::perspective_rh`.
fn perspective_off_center(
    tan_l: f32,
    tan_r: f32,
    tan_u: f32,
    tan_d: f32,
    near: f32,
    far: f32,
) -> Mat4 {
    let w = tan_r - tan_l;
    let h = tan_u - tan_d;
    let a00 = 2.0 / w;
    let a11 = 2.0 / h;
    let a20 = (tan_r + tan_l) / w;
    let a21 = (tan_u + tan_d) / h;
    let a22 = -far / (far - near);
    let a32 = -(far * near) / (far - near);
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

    /// The depth values chained into `CompositionLayerDepthInfoKHR`
    /// (`min_depth = 0.0`, `max_depth = 1.0`, `near_z = near`, `far_z = far`)
    /// must match what [`perspective_off_center`] actually writes into the depth
    /// buffer, or the compositor reprojects on wrong geometry. This locks the
    /// convention: a point on the near plane maps to NDC z = `min_depth` (0.0)
    /// and a point on the far plane maps to NDC z = `max_depth` (1.0), for the
    /// right-handed `z ∈ [0, 1]` projection (camera looks down -z).
    #[test]
    fn depth_range_matches_projection_convention() {
        const NEAR: f32 = 0.05;
        const FAR: f32 = 5000.0;
        // Symmetric 90° FOV tangents (±1); the depth mapping is independent of
        // the lateral FOV, so any valid tangents work.
        let proj = perspective_off_center(-1.0, 1.0, 1.0, -1.0, NEAR, FAR);

        // Project a view-space point at the given forward distance (-z) and
        // return its NDC z (clip.z / clip.w).
        let ndc_z = |dist: f32| {
            let clip = proj * Vec4::new(0.0, 0.0, -dist, 1.0);
            clip.z / clip.w
        };

        // near plane → min_depth (0.0)
        assert!(
            (ndc_z(NEAR) - 0.0).abs() < 1e-4,
            "near plane should map to min_depth 0.0, got {}",
            ndc_z(NEAR)
        );
        // far plane → max_depth (1.0)
        assert!(
            (ndc_z(FAR) - 1.0).abs() < 1e-4,
            "far plane should map to max_depth 1.0, got {}",
            ndc_z(FAR)
        );
        // Monotonic increase with distance (nearer = smaller depth).
        let mid = ndc_z((NEAR + FAR) * 0.5);
        assert!(mid > 0.0 && mid < 1.0, "mid depth {mid} out of (0,1)");
    }

    /// `eye_view_matrix` composes the player placement (`stage_to_world`) onto the
    /// located eye pose while keeping the eye's own rotation/translation (physical
    /// head motion + head-look) live. Identity placement reproduces the raw view;
    /// a translation moves the viewpoint by exactly that offset; a snap-turn yaw
    /// rotates the view on top of the HMD look rather than replacing it.
    #[test]
    fn eye_view_matrix_composes_player_placement_over_hmd_pose() {
        // A located eye pose with a non-trivial HMD orientation (yaw + pitch).
        let eye_rot = Quat::from_rotation_y(0.3) * Quat::from_rotation_x(-0.2);
        let eye_pos = Vec3::new(0.1, 1.6, -0.4);
        let eye_world = Mat4::from_rotation_translation(eye_rot, eye_pos);

        // (1) Identity placement → the raw located camera-to-world (view inverse).
        let c2w_id = eye_view_matrix(Mat4::IDENTITY, eye_world).inverse();
        assert!((c2w_id.transform_point3(Vec3::ZERO) - eye_pos).length() < 1e-5);
        let (_s, rot_id, _t) = c2w_id.to_scale_rotation_translation();
        assert!((rot_id * Vec3::NEG_Z - eye_rot * Vec3::NEG_Z).length() < 1e-5);

        // (2) Pure locomotion translation → viewpoint shifts by exactly that offset.
        let move_to = Vec3::new(10.0, 0.0, -5.0);
        let c2w_t = eye_view_matrix(Mat4::from_translation(move_to), eye_world).inverse();
        assert!((c2w_t.transform_point3(Vec3::ZERO) - (eye_pos + move_to)).length() < 1e-5);

        // (3) Snap-turn yaw → camera forward = yaw ∘ (HMD forward). The head-look
        // survives; the yaw rotates it on top (requirement 3).
        let yaw = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let c2w_y = eye_view_matrix(Mat4::from_quat(yaw), eye_world).inverse();
        let (_s, rot_y, _t) = c2w_y.to_scale_rotation_translation();
        let world_fwd = rot_y * Vec3::NEG_Z;
        let expected_fwd = yaw * (eye_rot * Vec3::NEG_Z);
        assert!(
            (world_fwd - expected_fwd).length() < 1e-5,
            "forward {world_fwd:?} should be yaw∘hmd {expected_fwd:?}"
        );
    }

    /// A play-space recenter must leave the headset view **unchanged**. The pawn
    /// root bakes `+R(yaw)·s` into its translation and the eye subtracts `s`; the
    /// yaw cancels through the composition (`M·T(−s)·raw = M0·raw`), so the
    /// rendered view is identical before and after a snap, for any yaw / basis.
    #[test]
    fn recenter_is_invariant_for_the_headset_view() {
        let basis = Mat4::from_scale(Vec3::new(1.0, 1.0, -1.0)); // non-trivial (handedness flip)
        let pos = Vec3::new(5.0, 0.0, -3.0); // locomotion
        let yaw = 0.7_f32;
        let r_yaw = Mat4::from_rotation_y(yaw);
        let s = Vec3::new(0.4, 0.0, -0.9); // recenter ground offset

        // Pre-recenter root vs the baked root (translation gains R(yaw)·s).
        let m0 = basis * Mat4::from_translation(pos) * r_yaw;
        let baked = Quat::from_rotation_y(yaw) * s;
        let m = basis * Mat4::from_translation(pos + baked) * r_yaw;

        // A raw located eye pose (head move + look).
        let eye_rot = Quat::from_rotation_y(0.2) * Quat::from_rotation_x(-0.1);
        let eye_pos = Vec3::new(0.3, 1.6, -0.2);
        let raw_eye = Mat4::from_rotation_translation(eye_rot, eye_pos);
        // The eye as `eye_camera` composes it under recenter: position − s.
        let comp_eye = Mat4::from_rotation_translation(eye_rot, eye_pos - s);

        let v_recentered = eye_view_matrix(m, comp_eye);
        let v_base = eye_view_matrix(m0, raw_eye);
        assert!(
            v_recentered.abs_diff_eq(v_base, 1e-4),
            "recenter must not move the view:\n{v_recentered:?}\nvs\n{v_base:?}"
        );
    }

    /// After a snap (`s = flatten(head)`) the static capsule — which gets no eye
    /// subtraction — lands under the head's ground projection, correctly even
    /// with a non-zero yaw (the yaw lives in the bake `R(yaw)·s`, not the eye).
    #[test]
    fn recenter_lands_capsule_under_head() {
        let pos = Vec3::new(2.0, 0.0, 1.0);
        let yaw = 0.9_f32;
        let r_yaw = Mat4::from_rotation_y(yaw);
        let head = Vec3::new(0.6, 1.7, -0.4); // raw head in play space
        let s = Vec3::new(head.x, 0.0, head.z); // snap: s = flatten(head)

        let m0 = Mat4::from_translation(pos) * r_yaw;
        let baked = Quat::from_rotation_y(yaw) * s;
        let m = Mat4::from_translation(pos + baked) * r_yaw;

        // Head ground (rendered via the pre-recenter root) vs capsule ground
        // (static child at (0, 0.85, 0) under the baked root).
        let head_world = m0.transform_point3(head);
        let capsule_world = m.transform_point3(Vec3::new(0.0, 0.85, 0.0));
        let head_ground = Vec3::new(head_world.x, 0.0, head_world.z);
        let capsule_ground = Vec3::new(capsule_world.x, 0.0, capsule_world.z);
        assert!(
            (head_ground - capsule_ground).length() < 1e-4,
            "capsule {capsule_ground:?} should sit under head {head_ground:?}"
        );
    }
}
