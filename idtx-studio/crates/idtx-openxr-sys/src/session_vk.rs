//! Vulkan graphics specifics for the real `OpenXR` session (Quest/Android).
//!
//! Only compiled on Android under a real-XR build (`idtx_xr` && `target_os =
//! "android"`). The Vulkan parallel to `session.rs` (D3D12): the
//! `khr_vulkan_enable2` instance, the `XR_KHR_vulkan_enable2` instance/device
//! creation passthroughs the runtime dictates, the VK swapchain formats, and
//! the raw `VkImage` swapchain-image accessors. Everything graphics-agnostic
//! (the frame loop, reference spaces, action set, the [`XrInstance`] /
//! [`XrGraphicsSession`] types) lives in `session_common.rs`, shared with D3D12.
//!
//! Unlike D3D12 — where wgpu creates the device independently and the LUID just
//! selects the adapter — Vulkan requires the runtime to be *in the loop* of
//! `VkInstance`/`VkDevice` creation (`xrCreateVulkanInstanceKHR` /
//! `xrCreateVulkanDeviceKHR`) so it can inject the extensions it needs. The
//! caller (`idtx-studio-session::xr_vulkan`) builds the ash `VkInstanceCreateInfo`
//! / `VkDeviceCreateInfo`, drives these passthroughs to let the runtime create
//! the real handles, then wraps those handles into wgpu-hal. All handles cross
//! this boundary as raw pointers / `u64` (openxr-sys models them as `c_void` /
//! `u64`), so this crate needs no `ash` dependency.
//!
//! # Not unit-testable
//!
//! Everything here needs a live runtime + headset; there are no unit tests.

use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};

use openxr as xr;
use openxr::Graphics as _;
use openxr::vulkan::{SessionCreateInfo as VkSessionCreateInfo, Vulkan};

use crate::XrError;
use crate::session_common::{XrGraphicsSession, XrInstance};

/// Process-global Android `JavaVM*` + **Activity** jobject, captured from the
/// live `AndroidApp` in `run_android` and read by [`create_instance_with_activity`].
///
/// WHY this exists: `android-activity` initialises [`ndk_context`] with the
/// **Application** object (`activity.getApplication()`), *not* the Activity. But
/// `openxr::Entry::create_instance` blindly feeds `ndk_context.context()` into
/// `XrInstanceCreateInfoAndroidKHR::applicationActivity`. The Meta Quest
/// standalone runtime needs the real **Activity** jobject there to observe the
/// activity lifecycle (resume / window focus); handed the Application it never
/// advances the session past `XR_SESSION_STATE_IDLE` — `xrPollEvent` returns
/// nothing, the frame loop free-runs without ever submitting a composition
/// layer, and the headset shows the system loading screen forever. We capture
/// the Activity from `AndroidApp::activity_as_ptr()` and build the instance by
/// hand so the correct handle crosses the boundary.
///
/// [`ndk_context`]: https://docs.rs/ndk-context
static ANDROID_VM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ANDROID_ACTIVITY: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Record the live Android `JavaVM*` and **Activity** jobject (from
/// `AndroidApp::vm_as_ptr()` / `AndroidApp::activity_as_ptr()`).
///
/// Call once from `android_main` **before** the `OpenXR` instance is created. The
/// pointers are process-global and valid for the process lifetime (the Activity
/// global reference lives as long as the app). When this is never called, the
/// instance falls back to `openxr::Entry::create_instance` (the Application
/// context) with a warning — see [`ANDROID_VM`] for why that stalls the session.
pub fn set_android_activity(vm: *mut c_void, activity: *mut c_void) {
    ANDROID_VM.store(vm, Ordering::SeqCst);
    ANDROID_ACTIVITY.store(activity, Ordering::SeqCst);
}

/// Build the [`openxr::AndroidPlatformInfo`] (JavaVM + **Activity** jobject)
/// recorded by [`set_android_activity`], or `None` when it was never called.
///
/// `OpenXR` 0.22 moved all Android-specific instance/loader plumbing behind
/// `AndroidPlatformInfo`: passed to `Entry::load` it drives
/// `xrInitializeLoaderKHR` (`init_loader`), and passed to
/// `Entry::create_instance` it chains `XrInstanceCreateInfoAndroidKHR` with the
/// Activity — exactly the hand-rolled work this module used to do. We feed it the
/// real **Activity** jobject (not the Application that `android-activity` stores in
/// `ndk_context`), which the Meta Quest runtime needs to track the activity
/// lifecycle; handed the Application the session never leaves IDLE (see
/// [`ANDROID_VM`]).
pub(crate) fn android_platform_info() -> Option<xr::AndroidPlatformInfo> {
    let vm = ANDROID_VM.load(Ordering::SeqCst);
    let activity = ANDROID_ACTIVITY.load(Ordering::SeqCst);
    if vm.is_null() || activity.is_null() {
        return None;
    }
    // SAFETY: both pointers were recorded by `set_android_activity` from the live
    // `AndroidApp` (`vm_as_ptr` / `activity_as_ptr`); the JavaVM and the Activity
    // global reference are valid for the process lifetime.
    Some(unsafe { xr::AndroidPlatformInfo::new(vm, activity) })
}

/// `VK_FORMAT_R8G8B8A8_SRGB` — the color swapchain format, chosen to map 1:1
/// onto wgpu `Rgba8UnormSrgb` so the copy from the render target is a straight
/// sRGB→sRGB blit (the Vulkan parallel to the D3D12 `DXGI_FORMAT_*` consts).
const VK_FORMAT_R8G8B8A8_SRGB: u32 = 43;

/// `VK_FORMAT_D32_SFLOAT` — the depth swapchain format for the
/// `XR_KHR_composition_layer_depth` layer, mapping 1:1 onto wgpu `Depth32Float`.
const VK_FORMAT_D32_SFLOAT: u32 = 126;

/// A Vulkan `vkGetInstanceProcAddr`-shaped loader entry point, supplied by the
/// caller's Vulkan loader (ash's static fn). Layout-identical to
/// `openxr::sys::platform::VkGetInstanceProcAddr`; the caller passes it in
/// type-erased as `*const c_void` and this module transmutes it, so neither
/// side needs the other's exact fn-pointer type.
pub type VkGetInstanceProcAddr =
    unsafe extern "system" fn(*const c_void, *const c_char) -> Option<unsafe extern "system" fn()>;

/// Pack an `OpenXR`-reported Vulkan API version into the
/// `VK_MAKE_API_VERSION(0, major, minor, 0)` encoding used by
/// `VkApplicationInfo::apiVersion`.
#[must_use]
fn vk_api_version(v: xr::Version) -> u32 {
    let major = u32::from(v.major());
    let minor = u32::from(v.minor());
    (major << 22) | (minor << 12)
}

impl XrInstance {
    /// Create a persistent instance with the `XR_KHR_vulkan_enable2` graphics
    /// extension and resolve the head-mounted-display system. The Vulkan
    /// parallel to [`XrInstance::create`] (D3D12).
    pub(crate) fn create_vk(entry: &xr::Entry) -> Result<Self, XrError> {
        let mut ext = xr::ExtensionSet::default();
        ext.khr_vulkan_enable2 = true;
        // REQUIRED on the Meta Quest standalone runtime: `XR_KHR_android_create_instance`
        // chains `XrInstanceCreateInfoAndroidKHR` (the JavaVM + activity jobject)
        // onto `xrCreateInstance`, binding the OpenXR instance to the foreground
        // Android activity. Without it `xrCreateSession` still succeeds but the
        // runtime never observes the activity lifecycle, so the session is stuck
        // at `IDLE` forever (never transitions to READY). We enable the extension
        // here and build the instance ourselves in `create_instance_with_activity`
        // below so the chained struct carries the *real* Activity jobject — NOT the
        // Application object that `android-activity` stores in `ndk_context` (which
        // `openxr::Entry::create_instance` would use, and which also leaves the
        // session stuck at IDLE on the Meta runtime).
        ext.khr_android_create_instance = true;
        // Probe once for the optional extensions we can use if advertised;
        // requesting an unavailable extension fails `create_instance` outright.
        let available = entry.enumerate_extensions().ok();
        let depth_layer_supported = available
            .as_ref()
            .is_some_and(|a| a.khr_composition_layer_depth);
        if depth_layer_supported {
            ext.khr_composition_layer_depth = true;
        }
        #[cfg(feature = "hand-tracking")]
        {
            if available.as_ref().is_some_and(|a| a.ext_hand_tracking) {
                ext.ext_hand_tracking = true;
            }
        }
        let app = xr::ApplicationInfo {
            application_name: "IDTX Studio",
            ..Default::default()
        };
        // Create the instance via `AndroidPlatformInfo` so
        // `XrInstanceCreateInfoAndroidKHR` carries the real **Activity** jobject
        // (see `android_platform_info`): the stock `()` path would chain nothing,
        // and the ndk_context Application the Meta Quest runtime rejects for
        // lifecycle tracking → the session never leaves IDLE. With no recorded
        // Activity we warn and fall back to `()` (no chained struct).
        let instance = match android_platform_info() {
            Some(pi) => {
                tracing::info!(
                    "creating OpenXR instance with the real Android Activity jobject \
                     chained into XrInstanceCreateInfoAndroidKHR"
                );
                entry.create_instance(&app, &ext, &[], &pi)
            }
            None => {
                tracing::warn!(
                    "no Android Activity recorded (set_android_activity was not called \
                     before XR init); creating the OpenXR instance without the Activity \
                     binding — the Quest session will likely stay stuck at IDLE"
                );
                entry.create_instance(&app, &ext, &[], &())
            }
        }
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

    /// The `(min, max)` Vulkan API versions the runtime supports, in
    /// `VK_MAKE_API_VERSION(0, major, minor, 0)` encoding. The caller picks an
    /// `apiVersion` in this range for its `VkInstanceCreateInfo`.
    pub fn vulkan_requirements(&self) -> Result<(u32, u32), XrError> {
        let req = Vulkan::requirements(&self.instance, self.system)
            .map_err(|e| XrError::GraphicsBindingFailed(format!("vulkan requirements: {e:?}")))?;
        Ok((
            vk_api_version(req.min_api_version_supported),
            vk_api_version(req.max_api_version_supported),
        ))
    }

    /// Create the runtime-blessed `VkInstance` (`xrCreateVulkanInstanceKHR`).
    /// [`Self::vulkan_requirements`] must be read first. Returns the raw
    /// `VkInstance` handle for the caller to wrap into wgpu-hal.
    ///
    /// # Safety
    ///
    /// `get_proc` must be a valid `vkGetInstanceProcAddr`, and `create_info` a
    /// valid `*const VkInstanceCreateInfo` whose referenced data outlives the
    /// call.
    pub unsafe fn create_vulkan_instance(
        &self,
        get_proc: *const c_void,
        create_info: *const c_void,
    ) -> Result<*const c_void, XrError> {
        // SAFETY: `get_proc` is a `vkGetInstanceProcAddr` fn pointer passed
        // type-erased; transmuting it back to the loader fn-pointer type is
        // layout-preserving (both are pointer-sized `extern "system"` fns).
        let get_proc: xr::sys::platform::VkGetInstanceProcAddr =
            unsafe { std::mem::transmute::<*const c_void, VkGetInstanceProcAddr>(get_proc) };
        // SAFETY: forwarded from this fn's own safety contract.
        let created = unsafe {
            self.instance
                .create_vulkan_instance(self.system, get_proc, create_info.cast())
        }
        .map_err(|e| XrError::GraphicsBindingFailed(format!("create_vulkan_instance: {e:?}")))?;
        created.map_err(|vk| {
            XrError::GraphicsBindingFailed(format!("vkCreateInstance failed (VkResult {vk})"))
        })
    }

    /// Select the `VkPhysicalDevice` the runtime requires for `vk_instance`
    /// (`xrGetVulkanGraphicsDevice2KHR`).
    ///
    /// # Safety
    ///
    /// `vk_instance` must be the `VkInstance` returned by
    /// [`Self::create_vulkan_instance`].
    pub unsafe fn vulkan_graphics_device(
        &self,
        vk_instance: *const c_void,
    ) -> Result<*const c_void, XrError> {
        // SAFETY: forwarded from this fn's own safety contract.
        unsafe {
            self.instance
                .vulkan_graphics_device(self.system, vk_instance)
        }
        .map_err(|e| XrError::GraphicsBindingFailed(format!("vulkan_graphics_device: {e:?}")))
    }

    /// Create the runtime-blessed `VkDevice` (`xrCreateVulkanDeviceKHR`).
    /// Returns the raw `VkDevice` handle for the caller to wrap into wgpu-hal.
    ///
    /// # Safety
    ///
    /// `get_proc` must be a valid `vkGetInstanceProcAddr`, `physical_device` the
    /// handle from [`Self::vulkan_graphics_device`], and `create_info` a valid
    /// `*const VkDeviceCreateInfo` whose referenced data outlives the call.
    pub unsafe fn create_vulkan_device(
        &self,
        get_proc: *const c_void,
        physical_device: *const c_void,
        create_info: *const c_void,
    ) -> Result<*const c_void, XrError> {
        // SAFETY: see `create_vulkan_instance` — layout-preserving transmute.
        let get_proc: xr::sys::platform::VkGetInstanceProcAddr =
            unsafe { std::mem::transmute::<*const c_void, VkGetInstanceProcAddr>(get_proc) };
        // SAFETY: forwarded from this fn's own safety contract.
        let created = unsafe {
            self.instance.create_vulkan_device(
                self.system,
                get_proc,
                physical_device,
                create_info.cast(),
            )
        }
        .map_err(|e| XrError::GraphicsBindingFailed(format!("create_vulkan_device: {e:?}")))?;
        created.map_err(|vk| {
            XrError::GraphicsBindingFailed(format!("vkCreateDevice failed (VkResult {vk})"))
        })
    }

    /// Create the graphics-bound session from the runtime-blessed Vulkan
    /// instance/physical-device/device + queue indices (the Vulkan parallel to
    /// [`XrInstance::create_session`]).
    ///
    /// # Safety
    ///
    /// All handles must be the ones obtained from this instance's
    /// `create_vulkan_*` / `vulkan_graphics_device` passthroughs, and must
    /// outlive the returned session. `queue_family_index` / `queue_index` must
    /// name a queue created on `vk_device`.
    pub unsafe fn create_session_vk(
        &self,
        vk_instance: *const c_void,
        physical_device: *const c_void,
        vk_device: *const c_void,
        queue_family_index: u32,
        queue_index: u32,
    ) -> Result<XrGraphicsSession<Vulkan>, XrError> {
        let info = VkSessionCreateInfo {
            instance: vk_instance,
            physical_device,
            device: vk_device,
            queue_family_index,
            queue_index,
        };
        // OpenXR mandates `xrGetVulkanGraphicsRequirements2KHR` for this
        // (instance, system) BEFORE `xrCreateSession`, else the runtime returns
        // `ERROR_GRAPHICS_REQUIREMENTS_CALL_MISSING`. Under Strategy A wgpu creates
        // the device (the runtime need not pick it), but the requirements query is
        // still required — make it here and discard the version range. (The D3D12
        // path satisfies the equivalent requirement via its `adapter_luid` →
        // `D3D12::requirements` call before session create.)
        let _req = Vulkan::requirements(&self.instance, self.system).map_err(|e| {
            XrError::GraphicsBindingFailed(format!("vulkan graphics requirements: {e:?}"))
        })?;
        // SAFETY: forwarded from this fn's own safety contract.
        let (session, waiter, stream) = unsafe {
            self.instance.create_session::<Vulkan>(self.system, &info)
        }
        .map_err(|e| XrError::GraphicsBindingFailed(format!("create_session (vulkan): {e:?}")))?;

        self.finish_session_setup::<Vulkan>(
            session,
            waiter,
            stream,
            VK_FORMAT_R8G8B8A8_SRGB,
            VK_FORMAT_D32_SFLOAT,
        )
    }
}

impl XrGraphicsSession<Vulkan> {
    /// The raw `VkImage` handle for every image in each eye's color swapchain.
    /// Enumerate once and cache the wrapped textures (the Vulkan parallel to the
    /// D3D12 `swapchain_image_ptrs`).
    pub fn swapchain_image_handles(&self) -> Result<[Vec<u64>; 2], XrError> {
        let a = self.swapchains[0]
            .enumerate_images()
            .map_err(|e| XrError::GraphicsBindingFailed(format!("enumerate_images: {e:?}")))?;
        let b = self.swapchains[1]
            .enumerate_images()
            .map_err(|e| XrError::GraphicsBindingFailed(format!("enumerate_images: {e:?}")))?;
        Ok([a, b])
    }

    /// The raw `VkImage` handle for every image in each eye's **depth**
    /// swapchain, or `None` when no depth layer is active.
    pub fn depth_swapchain_image_handles(&self) -> Result<Option<[Vec<u64>; 2]>, XrError> {
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
