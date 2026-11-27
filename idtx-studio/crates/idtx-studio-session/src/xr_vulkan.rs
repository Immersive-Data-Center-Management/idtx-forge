//! Strategy-B Vulkan bring-up for Quest: let the `OpenXR` runtime create the
//! `VkInstance`/`VkDevice` it dictates (`XR_KHR_vulkan_enable2`), then wrap those
//! handles into a wgpu device via wgpu-hal.
//!
//! The Meta Quest runtime rejects an app-created Vulkan device at
//! `xrCreateSession` (`ERROR_VALIDATION_FAILURE`): with `khr_vulkan_enable2` the
//! instance and device MUST be born from `xrCreateVulkanInstanceKHR` /
//! `xrCreateVulkanDeviceKHR` so the runtime can inject the extensions it needs.
//! So, unlike the D3D12 path (where wgpu builds the device and we hand the raw
//! pointers to `OpenXR`), here the order is inverted:
//!
//! 1. `xrGetVulkanGraphicsRequirements2KHR` → pick a Vulkan API version.
//! 2. Build a `VkInstanceCreateInfo` with wgpu-hal's desired instance extensions
//!    and hand it to `xrCreateVulkanInstanceKHR` → the runtime creates the
//!    `VkInstance`.
//! 3. `xrGetVulkanGraphicsDevice2KHR` → the `VkPhysicalDevice` the runtime wants.
//! 4. Build a `VkDeviceCreateInfo` with wgpu-hal's required device extensions +
//!    features and hand it to `xrCreateVulkanDeviceKHR` → the runtime creates the
//!    `VkDevice`.
//! 5. Wrap the three handles into wgpu-hal (`Instance::from_raw`,
//!    `expose_adapter`, `device_from_raw`) and lift to a `wgpu::Device`/`Queue`.
//!
//! The resulting [`VkBringup`] carries the wgpu device/queue (for rendering) plus
//! the raw handles (to hand to `XrInstance::create_session_vk`, which the runtime
//! now accepts because they are its own). Device-bound: only verifiable on a Quest.

use std::ffi::{c_char, c_void};

use ash::vk;
use ash::vk::Handle as _;
use idtx_openxr_sys::XrInstance;

use crate::xr_preview::XrPreviewError;

/// The Android API level reported to wgpu-hal for driver-workaround selection.
/// Quest floor is 30; a slightly-low value only widens workarounds, never breaks
/// correctness. (Querying the live `ro.build.version.sdk` is a possible refinement.)
const ANDROID_SDK_VERSION: u32 = 30;

/// Pack a `VK_MAKE_API_VERSION(0, major, minor, 0)` value.
const fn vk_make_version(major: u32, minor: u32) -> u32 {
    (major << 22) | (minor << 12)
}

/// Outcome of the `OpenXR`-dictated Vulkan bring-up: the wgpu device/queue to
/// render with, plus the raw handles to pass to `create_session_vk`.
pub struct VkBringup {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub vk_instance: *const c_void,
    pub physical_device: *const c_void,
    pub vk_device: *const c_void,
    pub queue_family_index: u32,
    pub queue_index: u32,
}

impl std::fmt::Debug for VkBringup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VkBringup")
            .field("queue_family_index", &self.queue_family_index)
            .field("queue_index", &self.queue_index)
            .finish_non_exhaustive()
    }
}

/// Create the runtime-blessed Vulkan instance+device and wrap them into wgpu.
///
/// # Errors
///
/// Returns [`XrPreviewError::Loader`] (wrapping a graphics-binding error) when
/// the Vulkan loader, any `OpenXR` create call, or the wgpu-hal wrap fails.
pub fn bring_up(xr: &XrInstance) -> Result<VkBringup, XrPreviewError> {
    use idtx_openxr_sys::XrError;
    use wgpu::hal::vulkan as hv;

    let gfx_err = |msg: String| XrError::GraphicsBindingFailed(msg);

    // 1. Load the Vulkan loader (libvulkan.so on Android) and pick an API version
    //    inside the runtime's supported range.
    let entry =
        unsafe { ash::Entry::load() }.map_err(|e| gfx_err(format!("load Vulkan loader: {e}")))?;
    let (min_api, max_api) = xr.vulkan_requirements()?;
    let want = vk_make_version(1, 1);
    let api_version = want.clamp(min_api, max_api.max(min_api));
    let get_proc = entry.static_fn().get_instance_proc_addr as *const c_void;
    let flags = wgpu::InstanceFlags::empty();

    // 2. Instance: enable the extensions wgpu-hal wants; the runtime appends its
    //    own and performs the actual vkCreateInstance.
    let instance_extensions = hv::Instance::desired_extensions(&entry, api_version, flags)
        .map_err(|e| gfx_err(format!("vulkan desired instance extensions: {e}")))?;
    let instance_ext_ptrs: Vec<*const c_char> =
        instance_extensions.iter().map(|c| c.as_ptr()).collect();
    let app_info = vk::ApplicationInfo::default().api_version(api_version);
    let instance_ci = vk::InstanceCreateInfo::default()
        .application_info(&app_info)
        .enabled_extension_names(&instance_ext_ptrs);
    let vk_instance_raw =
        unsafe { xr.create_vulkan_instance(get_proc, std::ptr::from_ref(&instance_ci).cast()) }?;
    let ash_instance = unsafe {
        ash::Instance::load(
            entry.static_fn(),
            vk::Instance::from_raw(vk_instance_raw as u64),
        )
    };

    // 3. The physical device the runtime dictates for this instance.
    let phys_raw = unsafe { xr.vulkan_graphics_device(vk_instance_raw) }?;
    let phys = vk::PhysicalDevice::from_raw(phys_raw as u64);

    // 4. Wrap the instance into wgpu-hal and expose the dictated adapter.
    let hal_instance = unsafe {
        hv::Instance::from_raw(
            entry.clone(),
            ash_instance.clone(),
            api_version,
            ANDROID_SDK_VERSION,
            None,
            instance_extensions.clone(),
            flags,
            wgpu::MemoryBudgetThresholds::default(),
            false,
            None,
        )
    }
    .map_err(|e| gfx_err(format!("wgpu-hal Instance::from_raw: {e}")))?;
    let exposed = hal_instance.expose_adapter(phys).ok_or_else(|| {
        gfx_err("wgpu-hal could not expose the OpenXR physical device".to_owned())
    })?;
    let features = exposed.features;

    // The high-level wgpu device enables only a narrow subset of the adapter's
    // features (the HAL device below is created with the full `features` set, so
    // any subset is valid). `POLYGON_MODE_LINE` backs wireframe rendering; the
    // GPU-timing query features are added only under the `metrics` feature and
    // only when the Adreno/Vulkan adapter actually reports them — without this,
    // the Quest renderer (built via `Renderer::from_device`) sees a device with
    // no timestamp support and every GPU-execution metric silently reads zero.
    // See docs/xr-quest-performance-analysis.md §4 Gap A.
    //
    // `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` is enabled when reported so the
    // renderer may use 2× MSAA on the `Rgba16Float` HDR target (QI-2); WebGPU core
    // only guarantees 1×/4× for that format. Without it `Renderer::from_device`
    // clamps the Android 2× default back up to 4×.
    let msaa_feature = features & wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
    #[cfg(feature = "metrics")]
    let enabled_features = (features & wgpu::Features::POLYGON_MODE_LINE)
        | msaa_feature
        | (features
            & (wgpu::Features::TIMESTAMP_QUERY
                | wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES
                | wgpu::Features::PIPELINE_STATISTICS_QUERY));
    #[cfg(not(feature = "metrics"))]
    let enabled_features = (features & wgpu::Features::POLYGON_MODE_LINE) | msaa_feature;

    // 5. Device: enable wgpu-hal's required device extensions + features, chosen
    //    for the exposed adapter; the runtime appends its own and performs the
    //    actual vkCreateDevice.
    let device_extensions = exposed.adapter.required_device_extensions(features);
    let device_ext_ptrs: Vec<*const c_char> =
        device_extensions.iter().map(|c| c.as_ptr()).collect();
    let queue_family_index = graphics_queue_family(&ash_instance, phys)
        .ok_or_else(|| gfx_err("no graphics-capable Vulkan queue family".to_owned()))?;
    let queue_priorities = [1.0_f32];
    let queue_ci = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(queue_family_index)
        .queue_priorities(&queue_priorities)];
    let mut phd_features = exposed
        .adapter
        .physical_device_features(&device_extensions, features);
    let device_ci = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queue_ci)
        .enabled_extension_names(&device_ext_ptrs);
    let device_ci = phd_features.add_to_device_create(device_ci);
    let vk_device_raw = unsafe {
        xr.create_vulkan_device(get_proc, phys_raw, std::ptr::from_ref(&device_ci).cast())
    }?;
    let ash_device = unsafe {
        ash::Device::load(
            ash_instance.fp_v1_0(),
            vk::Device::from_raw(vk_device_raw as u64),
        )
    };

    // 6. Wrap the device into wgpu-hal, then lift instance+adapter+device to the
    //    high-level wgpu handles the renderer uses.
    let open_device = unsafe {
        exposed.adapter.device_from_raw(
            ash_device,
            None,
            &device_extensions,
            features,
            &wgpu::Limits::default(),
            &wgpu::MemoryHints::default(),
            queue_family_index,
            0,
        )
    }
    .map_err(|e| gfx_err(format!("wgpu-hal device_from_raw: {e}")))?;

    let wgpu_instance = unsafe { wgpu::Instance::from_hal::<wgpu::hal::api::Vulkan>(hal_instance) };
    let wgpu_adapter =
        unsafe { wgpu_instance.create_adapter_from_hal::<wgpu::hal::api::Vulkan>(exposed) };
    let (device, queue) = unsafe {
        wgpu_adapter.create_device_from_hal::<wgpu::hal::api::Vulkan>(
            open_device,
            &wgpu::DeviceDescriptor {
                label: Some("IDTX Studio (Quest XR)"),
                required_features: enabled_features,
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            },
        )
    }
    .map_err(|e| gfx_err(format!("wgpu create_device_from_hal: {e}")))?;

    Ok(VkBringup {
        device,
        queue,
        vk_instance: vk_instance_raw,
        physical_device: phys_raw,
        vk_device: vk_device_raw,
        queue_family_index,
        queue_index: 0,
    })
}

/// The first queue family on `phys` that supports graphics.
fn graphics_queue_family(instance: &ash::Instance, phys: vk::PhysicalDevice) -> Option<u32> {
    let families = unsafe { instance.get_physical_device_queue_family_properties(phys) };
    families
        .iter()
        .position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        .map(|i| u32::try_from(i).unwrap_or(0))
}

// SAFETY: `VkBringup` carries raw Vulkan handles as `*const c_void` only so the
// caller can forward them to `create_session_vk` on the same thread immediately
// after bring-up; it is never sent across threads. The raw pointers are opaque
// handles, not dereferenced in Rust.
unsafe impl Send for VkBringup {}
