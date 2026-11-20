//! D3D12 graphics specifics for the real `OpenXR` session (Windows PCVR).
//!
//! Only compiled on Windows under a real-XR build (`idtx_xr` && `windows`).
//! Owns the D3D12 graphics binding at session-create time, the DXGI swapchain
//! formats, and the raw `ID3D12Resource*` swapchain-image accessors. Everything
//! graphics-API-agnostic (the frame loop, reference spaces, action set, and the
//! [`XrInstance`] / [`XrGraphicsSession`] types themselves) lives in
//! `session_common.rs`, shared with the Vulkan backend (`session_vk.rs`).
//!
//! All raw D3D12 handles cross the boundary as `*mut c_void` (openxr-sys models
//! `ID3D12Device` / `ID3D12CommandQueue` / `ID3D12Resource` as `c_void`), so
//! this crate needs no `windows` dependency — the caller wraps them via
//! wgpu-hal.
//!
//! # Not unit-testable
//!
//! Everything here needs a live runtime + headset; there are no unit tests.
//! The mock/headless seam stays in `idtx-platform-xr` (`mock-xr`).

use std::ffi::c_void;

use openxr as xr;
use openxr::Graphics as _;
use openxr::d3d::{D3D12, SessionCreateInfoD3D12};

use crate::XrError;
use crate::session_common::{XrGraphicsSession, XrInstance};

/// `DXGI_FORMAT_R8G8B8A8_UNORM_SRGB` — the swapchain format, chosen to map 1:1
/// onto wgpu `Rgba8UnormSrgb` so the copy from the render target is a straight
/// sRGB→sRGB blit.
const DXGI_FORMAT_R8G8B8A8_UNORM_SRGB: u32 = 29;

/// `DXGI_FORMAT_D32_FLOAT` — the depth swapchain format for the
/// `XR_KHR_composition_layer_depth` layer, chosen to map 1:1 onto wgpu
/// `Depth32Float` so the resolved single-sample depth copies straight in.
const DXGI_FORMAT_D32_FLOAT: u32 = 40;

impl XrInstance {
    /// Create a persistent instance with the D3D12 graphics extension and
    /// resolve the head-mounted-display system.
    pub(crate) fn create(entry: &xr::Entry) -> Result<Self, XrError> {
        let mut ext = xr::ExtensionSet::default();
        ext.khr_d3d12_enable = true;
        // Probe once for the optional extensions we can use if advertised;
        // requesting an unavailable extension fails `create_instance` outright.
        let available = entry.enumerate_extensions().ok();
        // `XR_KHR_composition_layer_depth`: lets us submit a per-pixel depth
        // layer so the compositor reprojects (ASW) on real geometry instead of
        // a flat plane — fixes the near-object (controller/hand) smear.
        let depth_layer_supported = available
            .as_ref()
            .is_some_and(|a| a.khr_composition_layer_depth);
        if depth_layer_supported {
            ext.khr_composition_layer_depth = true;
        }
        #[cfg(feature = "hand-tracking")]
        {
            // Only request the extension if the runtime advertises it; otherwise
            // `create_instance` fails with `XR_ERROR_EXTENSION_NOT_PRESENT`.
            if available.as_ref().is_some_and(|a| a.ext_hand_tracking) {
                ext.ext_hand_tracking = true;
            }
        }
        let app = xr::ApplicationInfo {
            application_name: "IDTX Studio",
            ..Default::default()
        };
        let instance = entry
            .create_instance(&app, &ext, &[], &())
            .map_err(|e| XrError::CreateInstanceFailed(format!("{e:?}")))?;
        let system = instance
            .system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)
            .map_err(|e| XrError::CreateInstanceFailed(format!("system: {e:?}")))?;
        Ok(Self {
            instance,
            system,
            depth_layer_supported,
        })
    }

    /// The DXGI adapter LUID the runtime requires the D3D12 device to use,
    /// as `(LowPart, HighPart)`.
    pub fn adapter_luid(&self) -> Result<(u32, i32), XrError> {
        let req = D3D12::requirements(&self.instance, self.system)
            .map_err(|e| XrError::GraphicsBindingFailed(format!("d3d12 requirements: {e:?}")))?;
        Ok((req.adapter_luid.LowPart, req.adapter_luid.HighPart))
    }

    /// Create the graphics-bound session (+ per-eye swapchains) from an
    /// existing D3D12 device+queue.
    ///
    /// # Safety
    ///
    /// `device` and `queue` must be valid `ID3D12Device*` / `ID3D12CommandQueue*`
    /// that outlive the returned session and belong to the adapter whose LUID
    /// matches [`Self::adapter_luid`]. The runtime AddRefs them internally.
    pub unsafe fn create_session(
        &self,
        device: *mut c_void,
        queue: *mut c_void,
    ) -> Result<XrGraphicsSession<D3D12>, XrError> {
        let info = SessionCreateInfoD3D12 {
            device: device.cast(),
            queue: queue.cast(),
        };
        // SAFETY: forwarded from this fn's own safety contract.
        let (session, waiter, stream) =
            unsafe { self.instance.create_session::<D3D12>(self.system, &info) }
                .map_err(|e| XrError::GraphicsBindingFailed(format!("create_session: {e:?}")))?;

        self.finish_session_setup::<D3D12>(
            session,
            waiter,
            stream,
            DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
            DXGI_FORMAT_D32_FLOAT,
        )
    }
}

impl XrGraphicsSession<D3D12> {
    /// The raw `ID3D12Resource*` (as `*mut c_void`) for every image in each
    /// eye's swapchain. Enumerate once and cache the wrapped textures.
    pub fn swapchain_image_ptrs(&self) -> Result<[Vec<*mut c_void>; 2], XrError> {
        let a = self.swapchains[0]
            .enumerate_images()
            .map_err(|e| XrError::GraphicsBindingFailed(format!("enumerate_images: {e:?}")))?;
        let b = self.swapchains[1]
            .enumerate_images()
            .map_err(|e| XrError::GraphicsBindingFailed(format!("enumerate_images: {e:?}")))?;
        Ok([a, b])
    }

    /// The raw `ID3D12Resource*` (as `*mut c_void`) for every image in each
    /// eye's **depth** swapchain, or `None` when no depth layer is active.
    /// Enumerate once and cache the wrapped textures (mirrors
    /// [`Self::swapchain_image_ptrs`]).
    pub fn depth_swapchain_image_ptrs(&self) -> Result<Option<[Vec<*mut c_void>; 2]>, XrError> {
        let Some(depth) = self.depth_swapchains.as_ref() else {
            return Ok(None);
        };
        let a = depth[0].enumerate_images().map_err(|e| {
            XrError::GraphicsBindingFailed(format!("depth enumerate_images: {e:?}"))
        })?;
        let b = depth[1].enumerate_images().map_err(|e| {
            XrError::GraphicsBindingFailed(format!("depth enumerate_images: {e:?}"))
        })?;
        Ok(Some([a, b]))
    }
}
