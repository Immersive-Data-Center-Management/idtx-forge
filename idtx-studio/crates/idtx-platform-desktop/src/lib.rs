//! Narrow winit + wgpu desktop lifecycle adapter for IDTX Studio.

#[cfg(not(target_os = "visionos"))]
use std::sync::Arc;

use thiserror::Error;
#[cfg(not(target_os = "visionos"))]
use winit::window::Window;

#[derive(Debug, Error)]
pub enum DesktopGpuError {
    #[error("failed to create wgpu surface: {0}")]
    Surface(#[from] wgpu::CreateSurfaceError),
    #[error("no suitable wgpu adapter found: {0}")]
    NoAdapter(#[source] wgpu::RequestAdapterError),
    #[error("no DX12 adapter matched the OpenXR-required LUID")]
    NoLuidMatch,
    #[error("failed to create wgpu device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
}

/// Shared wgpu device, queue, and (optional) swapchain surface for a window.
///
/// `surface` is `None` on the surfaceless Quest XR path (`from_wgpu`): the device
/// is created by the `OpenXR` runtime and output goes to the XR swapchains, not a
/// window swapchain, so there is nothing to present to.
#[derive(Debug)]
pub struct DesktopGpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface: Option<wgpu::Surface<'static>>,
    pub configuration: wgpu::SurfaceConfiguration,
}

// Docs here are dense with OpenXR / DXGI / D3D12 identifiers that read fine
// unbackticked; the AddRef/LUID terminology likewise.
#[allow(clippy::doc_markdown)]
impl DesktopGpu {
    /// Initialises a wgpu instance, adapter, device, and surface for `window`.
    ///
    /// When `preferred_luid` is `Some((low, high))` (an `OpenXR` D3D12 adapter
    /// LUID), the instance is forced onto the **DX12** backend and the adapter
    /// whose DXGI LUID matches is selected — required so the `OpenXR` runtime and
    /// wgpu share the same GPU (and so the D3D12 device handle can be handed to
    /// `xrCreateSession`). When `None`, the original default-backend
    /// `request_adapter` path is used unchanged.
    #[cfg(not(target_os = "visionos"))]
    pub async fn new(
        window: Arc<Window>,
        preferred_luid: Option<(u32, i32)>,
    ) -> Result<Self, DesktopGpuError> {
        let backends = if preferred_luid.is_some() {
            wgpu::Backends::DX12
        } else if cfg!(target_os = "android") {
            // Quest/Android has no DX12; the runtime renders over Vulkan. The
            // LUID/DX12 path above is Windows-only and never taken here.
            wgpu::Backends::VULKAN
        } else {
            wgpu::Backends::all()
        };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance.create_surface(window.clone())?;
        let adapter = match preferred_luid {
            Some(luid) => Self::select_adapter_by_luid(&instance, luid).await?,
            None => instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                })
                .await
                .map_err(DesktopGpuError::NoAdapter)?,
        };

        // The portable `Limits::default()` caps `max_buffer_size` at 256 MiB — a
        // WebGL2-era conservatism, not a real GPU limit. Large stages fold into a
        // single merged vertex buffer that can exceed that (e.g. ~597 MiB), so we
        // raise the buffer-size ceilings to whatever the adapter actually reports
        // (multi-GiB on Metal/DX12/Vulkan) while keeping every other default.
        let adapter_limits = adapter.limits();
        // Enable optional features only when the adapter advertises them, so this
        // never fails device creation on constrained hardware. POLYGON_MODE_LINE
        // backs wireframe; TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES lets the
        // renderer use 2× MSAA on `Rgba16Float` (QI-2, e.g. an `IDTX_XR_MSAA=2`
        // desktop/PCVR A/B — WebGPU core only guarantees 1×/4× for that format).
        let mut optional_features = wgpu::Features::empty();
        for f in [
            wgpu::Features::POLYGON_MODE_LINE,
            wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES,
        ] {
            if adapter.features().contains(f) {
                optional_features |= f;
            }
        }
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("IDTX Studio"),
                required_features: optional_features,
                required_limits: wgpu::Limits {
                    max_buffer_size: adapter_limits.max_buffer_size,
                    max_storage_buffer_binding_size: adapter_limits.max_storage_buffer_binding_size,
                    ..wgpu::Limits::default()
                },
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await?;

        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);

        let configuration = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &configuration);

        Ok(Self {
            device,
            queue,
            surface: Some(surface),
            configuration,
        })
    }

    /// Wrap an already-created wgpu device + queue with **no** window surface,
    /// for the Quest XR path where the `OpenXR` runtime creates the Vulkan device
    /// (`idtx-studio-session::xr_vulkan`) and all output goes to the XR
    /// swapchains. The carried `configuration` names a plausible sRGB format for
    /// any offscreen setup; it is never used to configure a (nonexistent)
    /// surface. `resize` / `set_present_mode` / window present are all no-ops
    /// while `surface` is `None`.
    #[cfg(all(target_os = "android", feature = "quest"))]
    #[must_use]
    pub fn from_wgpu(device: wgpu::Device, queue: wgpu::Queue, width: u32, height: u32) -> Self {
        let configuration = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::AutoNoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        Self {
            device,
            queue,
            surface: None,
            configuration,
        }
    }

    /// Enumerate DX12 adapters and pick the one whose DXGI LUID matches
    /// `(low, high)`. Falls back to the first DX12 adapter when none matches
    /// (with a warning) so a LUID quirk never hard-blocks startup.
    #[cfg(windows)]
    async fn select_adapter_by_luid(
        instance: &wgpu::Instance,
        luid: (u32, i32),
    ) -> Result<wgpu::Adapter, DesktopGpuError> {
        let adapters = instance.enumerate_adapters(wgpu::Backends::DX12).await;
        if adapters.is_empty() {
            return Err(DesktopGpuError::NoLuidMatch);
        }
        for adapter in &adapters {
            if adapter_luid(adapter) == Some(luid) {
                // Re-request to move ownership out of the borrowed Vec.
                return Ok(instance
                    .enumerate_adapters(wgpu::Backends::DX12)
                    .await
                    .into_iter()
                    .find(|a| adapter_luid(a) == Some(luid))
                    .expect("matching adapter present on re-enumerate"));
            }
        }
        eprintln!("[xr] no DX12 adapter matched OpenXR LUID {luid:?}; using first DX12 adapter");
        Ok(adapters.into_iter().next().expect("adapters non-empty"))
    }

    #[cfg(not(windows))]
    #[allow(clippy::unused_async)]
    async fn select_adapter_by_luid(
        _instance: &wgpu::Instance,
        _luid: (u32, i32),
    ) -> Result<wgpu::Adapter, DesktopGpuError> {
        Err(DesktopGpuError::NoLuidMatch)
    }

    /// Extract the raw `ID3D12Device*` and `ID3D12CommandQueue*` backing this
    /// wgpu device, as `*mut c_void`, for `xrCreateSession` (D3D12 binding).
    ///
    /// Returns `None` if wgpu is not running on the DX12 backend. The pointers
    /// are borrowed from wgpu's own COM objects; the caller must not outlive
    /// this `DesktopGpu`. The `OpenXR` runtime AddRefs them internally.
    #[cfg(windows)]
    #[must_use]
    pub fn d3d12_device_queue_ptrs(
        &self,
    ) -> Option<(*mut std::ffi::c_void, *mut std::ffi::c_void)> {
        use windows::core::Interface as _;
        // SAFETY: as_hal only exposes the backing handles; we read (not mutate)
        // the COM pointers and do not free them.
        unsafe {
            let hal = self.device.as_hal::<wgpu::hal::api::Dx12>()?;
            let device_ptr = hal.raw_device().as_raw();
            let queue_ptr = hal.raw_queue().as_raw();
            Some((device_ptr, queue_ptr))
        }
    }

    /// Extract the raw Vulkan handles backing this wgpu device — `VkInstance`,
    /// `VkPhysicalDevice`, `VkDevice` (as `*const c_void`) plus the graphics
    /// queue family/index — for the `OpenXR` Vulkan graphics binding
    /// (`xrCreateSession`). The Vulkan parallel to [`Self::d3d12_device_queue_ptrs`].
    ///
    /// Returns `None` if wgpu is not running on the Vulkan backend. The handles
    /// are borrowed from wgpu's own objects; the caller must not outlive this
    /// `DesktopGpu`. On Quest there is a single GPU, so the device wgpu created
    /// is the one the runtime drives the headset with.
    #[cfg(all(target_os = "android", feature = "quest"))]
    #[must_use]
    pub fn vulkan_handles(&self) -> Option<VulkanHandles> {
        use ash::vk::Handle as _;
        // SAFETY: as_hal only borrows the backing Vulkan objects; we read (never
        // mutate or free) the handles and hand their raw values to OpenXR.
        unsafe {
            let hal = self.device.as_hal::<wgpu::hal::api::Vulkan>()?;
            let instance =
                hal.shared_instance().raw_instance().handle().as_raw() as *const std::ffi::c_void;
            let physical_device = hal.raw_physical_device().as_raw() as *const std::ffi::c_void;
            let device = hal.raw_device().handle().as_raw() as *const std::ffi::c_void;
            Some(VulkanHandles {
                instance,
                physical_device,
                device,
                queue_family_index: hal.queue_family_index(),
                queue_index: hal.queue_index(),
            })
        }
    }

    /// Create a **surfaceless** wgpu device on the Metal backend for the Apple
    /// Vision Pro immersive runtime.
    ///
    /// visionOS has no window surface — rendering targets `CompositorServices`
    /// drawable textures, not a window swapchain — so this mirrors the
    /// surfaceless Quest path ([`from_wgpu`](Self::from_wgpu)) but creates the
    /// device itself on the Metal backend. On single-GPU Apple hardware wgpu
    /// resolves the system-default `MTLDevice`, which is the **same** device
    /// object `CompositorServices`' `LayerRenderer` uses; the Swift shell verifies
    /// this against [`metal_handles`](Self::metal_handles) so a drawable's
    /// `MTLTexture` can be imported into this device (Metal textures are not
    /// shareable across distinct `MTLDevice` objects). The carried
    /// `configuration` names a plausible sRGB format for offscreen setup; there
    /// is no surface to configure.
    #[cfg(target_vendor = "apple")]
    pub async fn new_metal_offscreen(width: u32, height: u32) -> Result<Self, DesktopGpuError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(DesktopGpuError::NoAdapter)?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("IDTX Studio (Vision Pro)"),
                required_features: {
                    // Base: wireframe polygon mode when the adapter has it.
                    let mut f = if adapter
                        .features()
                        .contains(wgpu::Features::POLYGON_MODE_LINE)
                    {
                        wgpu::Features::POLYGON_MODE_LINE
                    } else {
                        wgpu::Features::empty()
                    };
                    // Metrics: GPU timestamp queries so `idtx-render`'s
                    // `RenderFrameTimings.gpu_*_ns` are non-zero on this shared
                    // Metal device. Request only what the adapter advertises — a
                    // device whose required features exceed the adapter's is
                    // rejected. The within-pass split (`TIMESTAMP_QUERY_INSIDE_PASSES`)
                    // is often unavailable on Apple GPUs; whole-pass timing still
                    // works without it. Mirrors the owned-adapter `Renderer::new`.
                    #[cfg(feature = "metrics")]
                    {
                        let avail = adapter.features();
                        if avail.contains(wgpu::Features::TIMESTAMP_QUERY) {
                            f |= wgpu::Features::TIMESTAMP_QUERY;
                            if avail.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES) {
                                f |= wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES;
                            }
                        }
                    }
                    f
                },
                // Request exactly what the adapter supports rather than the
                // portable `Limits::default()`: the visionOS Simulator's Metal
                // reports a lower `max_inter_stage_shader_variables` (15) than the
                // default's 15-vs-16 — and a device whose required limits exceed
                // the adapter's is rejected. `adapter.limits()` is always a valid
                // request and still carries the device's real (multi-GiB)
                // buffer-size ceilings for large merged vertex buffers.
                required_limits: adapter.limits(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await?;

        let configuration = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::AutoNoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        Ok(Self {
            device,
            queue,
            surface: None,
            configuration,
        })
    }

    /// Extract the raw `MTLDevice` + `MTLCommandQueue` protocol-object pointers
    /// backing this wgpu device, as `*mut c_void`, for the Apple Vision Pro
    /// `CompositorServices` graphics binding — the Metal parallel to
    /// [`d3d12_device_queue_ptrs`](Self::d3d12_device_queue_ptrs) and
    /// [`vulkan_handles`](Self::vulkan_handles).
    ///
    /// Returns `None` if wgpu is not running on the Metal backend. The pointers
    /// are borrowed from wgpu's own retained objc2 objects; the caller must not
    /// outlive this `DesktopGpu` and must not release them. The Swift shell
    /// compares `device` against its `LayerRenderer.device` to confirm the
    /// shared-device invariant that makes drawable-texture import valid.
    #[cfg(target_vendor = "apple")]
    #[must_use]
    pub fn metal_handles(&self) -> Option<MetalHandles> {
        use objc2::rc::Retained;
        // SAFETY: as_hal only borrows the backing Metal objects; we read (never
        // mutate or release) the protocol-object pointers and hand their raw
        // values to the CompositorServices Swift shell, which retains them.
        unsafe {
            let hal_device = self.device.as_hal::<wgpu::hal::api::Metal>()?;
            let device = Retained::as_ptr(hal_device.raw_device())
                .cast::<std::ffi::c_void>()
                .cast_mut();
            let hal_queue = self.queue.as_hal::<wgpu::hal::api::Metal>()?;
            let command_queue = std::ptr::from_ref(hal_queue.as_raw())
                .cast::<std::ffi::c_void>()
                .cast_mut();
            Some(MetalHandles {
                device,
                command_queue,
            })
        }
    }

    /// Reconfigures the surface after a window resize. No-op when surfaceless.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.configuration.width = width;
        self.configuration.height = height;
        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &self.configuration);
        }
    }

    /// Switches the swapchain present mode and reconfigures the surface.
    ///
    /// The default is [`wgpu::PresentMode::AutoVsync`]. During an `OpenXR`
    /// session the desktop mirror should drop to [`wgpu::PresentMode::AutoNoVsync`]
    /// so the frame loop is paced by `xrWaitFrame` at the headset's native rate
    /// rather than the desktop refresh — submitting below the headset rate makes
    /// the compositor reproject every frame and smears near-object parallax.
    /// Restore [`wgpu::PresentMode::AutoVsync`] when the session ends.
    ///
    /// No-op when the mode is unchanged.
    pub fn set_present_mode(&mut self, present_mode: wgpu::PresentMode) {
        if self.configuration.present_mode == present_mode {
            return;
        }
        self.configuration.present_mode = present_mode;
        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &self.configuration);
        }
    }
}

/// Raw Vulkan handles backing a wgpu device, for the `OpenXR` Vulkan graphics
/// binding. Dispatchable handles (`instance`/`physical_device`/`device`) cross
/// as `*const c_void` to match openxr-sys's platform types; the two indices
/// name the graphics queue created on `device`.
#[cfg(all(target_os = "android", feature = "quest"))]
#[derive(Debug, Clone, Copy)]
pub struct VulkanHandles {
    pub instance: *const std::ffi::c_void,
    pub physical_device: *const std::ffi::c_void,
    pub device: *const std::ffi::c_void,
    pub queue_family_index: u32,
    pub queue_index: u32,
}

/// Read a wgpu adapter's DXGI LUID as `(LowPart, HighPart)` via wgpu-hal.
#[cfg(windows)]
fn adapter_luid(adapter: &wgpu::Adapter) -> Option<(u32, i32)> {
    // SAFETY: as_hal only borrows the backing DXGI adapter; GetDesc is a pure
    // read. Returns None for non-DX12 adapters.
    unsafe {
        let hal = adapter.as_hal::<wgpu::hal::api::Dx12>()?;
        let desc = hal.raw_adapter().GetDesc().ok()?;
        Some((desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart))
    }
}

/// Raw Metal handles backing a wgpu device, for the Apple Vision Pro
/// `CompositorServices` graphics binding. Both the `MTLDevice` and its
/// `MTLCommandQueue` cross as `*mut c_void` to match the C-ABI the Swift shell
/// receives; the Rust side borrows (never releases) them. The Metal parallel to
/// [`VulkanHandles`].
#[cfg(target_vendor = "apple")]
#[derive(Debug, Clone, Copy)]
#[allow(clippy::doc_markdown)]
pub struct MetalHandles {
    /// The `id<MTLDevice>` backing the wgpu Metal device.
    pub device: *mut std::ffi::c_void,
    /// The `id<MTLCommandQueue>` wgpu submits on.
    pub command_queue: *mut std::ffi::c_void,
}

#[cfg(all(test, target_vendor = "apple"))]
mod metal_tests {
    use super::*;

    /// The Metal GPU-handle path: a surfaceless Metal device must yield non-null
    /// `MTLDevice`/`MTLCommandQueue` pointers through [`DesktopGpu::metal_handles`].
    /// Validates Deliverable 1 on the macOS Metal backend without a window.
    #[test]
    fn metal_handles_extractable_from_offscreen_device() {
        let gpu = pollster::block_on(DesktopGpu::new_metal_offscreen(64, 64))
            .expect("create a surfaceless Metal device");
        let handles = gpu
            .metal_handles()
            .expect("Metal handles available on the Metal backend");
        assert!(
            !handles.device.is_null(),
            "MTLDevice pointer must be non-null"
        );
        assert!(
            !handles.command_queue.is_null(),
            "MTLCommandQueue pointer must be non-null"
        );
    }
}
