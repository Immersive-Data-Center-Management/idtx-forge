//! Real-device `OpenXR` orchestration for the runtime player.
//!
//! This mirrors the startup + start sequence the editor shell used to own
//! (`idtx-studio-shell`), relocated here so the editor no longer links any XR
//! code. Compiled on any real-XR build (`idtx_xr`): D3D12 on Windows PCVR,
//! Vulkan on Quest/Android. Every other build runs the player in
//! [`RunMode::Desktop`][crate::RunMode::Desktop].
//!
//! The persistent `XrInstance` is created first ([`XrHost::probe`]). On D3D12
//! its adapter LUID pins `DesktopGpu` onto the matching adapter
//! ([`XrHost::preferred_luid`] → `DesktopGpu::new`); on Vulkan the single Quest
//! GPU is used directly. [`XrHost::start`] then binds the graphics session once
//! the GPU + stage are open — handing wgpu's D3D12 device/queue, or its raw
//! Vulkan handles, to `xrCreateSession`.

use idtx_platform_desktop::DesktopGpu;
use idtx_studio_session::{StudioSession, XrInstance, XrLoader, probe_xr_runtime};

/// The runtime-created Vulkan handles from the Quest XR bring-up, stashed so
/// [`XrHost::start`] can hand them to `xrCreateSession` after the GPU + stage are
/// open. Raw opaque handles used on one thread only (see `xr_vulkan::VkBringup`).
#[cfg(all(idtx_xr, target_os = "android"))]
#[derive(Clone, Copy)]
struct VkHandles {
    instance: *const std::ffi::c_void,
    physical_device: *const std::ffi::c_void,
    device: *const std::ffi::c_void,
    queue_family_index: u32,
    queue_index: u32,
}

/// Owns the `OpenXR` loader + persistent instance for a runtime XR session.
pub struct XrHost {
    loader: XrLoader,
    instance: XrInstance,
    /// Quest only: handles from the `OpenXR`-dictated Vulkan bring-up, set by
    /// [`XrHost::bring_up_vulkan`] and consumed by [`XrHost::start`].
    #[cfg(all(idtx_xr, target_os = "android"))]
    vk: Option<VkHandles>,
}

impl std::fmt::Debug for XrHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XrHost").finish_non_exhaustive()
    }
}

impl XrHost {
    /// Probe for an `OpenXR` runtime and create the persistent graphics-capable
    /// instance (D3D12 on Windows, Vulkan on Quest). Returns `None` when no
    /// runtime is present (the player then falls back to desktop mode).
    #[must_use]
    pub fn probe() -> Option<Self> {
        let loader = probe_xr_runtime()?;
        #[cfg(all(idtx_xr, windows))]
        let created = loader.create_instance();
        #[cfg(all(idtx_xr, target_os = "android"))]
        let created = loader.create_instance_vk();
        match created {
            Ok(instance) => Some(Self {
                loader,
                instance,
                #[cfg(all(idtx_xr, target_os = "android"))]
                vk: None,
            }),
            Err(e) => {
                tracing::warn!("OpenXR instance creation failed: {e}");
                None
            }
        }
    }

    /// Quest: let the `OpenXR` runtime create the `VkInstance`/`VkDevice` it
    /// dictates (`XR_KHR_vulkan_enable2`), wrap them into a wgpu device, and stash
    /// the raw handles for [`XrHost::start`]. Returns the wgpu device + queue the
    /// player wraps in a surfaceless [`DesktopGpu`]. Must be called before the
    /// stage opens and before [`XrHost::start`].
    ///
    /// This replaces the D3D12 "wgpu builds the device, hand pointers to `OpenXR`"
    /// order: the Meta runtime rejects an app-created Vulkan device at
    /// `xrCreateSession`, so `OpenXR` must create it (`idtx-studio-session::xr_vulkan`).
    ///
    /// # Errors
    ///
    /// Returns a message when the Vulkan loader, any `OpenXR` create call, or the
    /// wgpu-hal wrap fails.
    #[cfg(all(idtx_xr, target_os = "android"))]
    pub fn bring_up_vulkan(&mut self) -> Result<(wgpu::Device, wgpu::Queue), String> {
        let bringup = idtx_studio_session::xr_vulkan::bring_up(&self.instance)
            .map_err(|e| format!("Vulkan XR bring-up failed: {e}"))?;
        self.vk = Some(VkHandles {
            instance: bringup.vk_instance,
            physical_device: bringup.physical_device,
            device: bringup.vk_device,
            queue_family_index: bringup.queue_family_index,
            queue_index: bringup.queue_index,
        });
        Ok((bringup.device, bringup.queue))
    }

    /// The adapter LUID the `OpenXR` runtime requires, to pin `DesktopGpu` onto
    /// the matching DX12 adapter before session creation (Windows PCVR only;
    /// the Quest/Vulkan path uses the single GPU directly).
    #[cfg(all(idtx_xr, windows))]
    #[must_use]
    pub fn preferred_luid(&self) -> Option<(u32, i32)> {
        self.instance.adapter_luid().ok()
    }

    /// The manifest device id (`run.toml`'s `[xr.controllers.<id>]` key) of the
    /// connected controller pair, e.g. `"quest3"` / `"questpro"` / `"generic"`.
    ///
    /// Resolved from the live `OpenXR` system so the packaged runtime can pick the
    /// matching controller mesh the same way the editor preview does. Classifies
    /// from the **persistent instance** this host already owns (not a fresh bare
    /// instance): over Quest Link a second instance in the same process degrades
    /// its system properties and the classifier would fall through to
    /// `"generic"`. Any probe failure degrades to the `"generic"` device (the
    /// manifest default), never an error — a missing mesh only means the runtime
    /// capsule fallback applies.
    #[must_use]
    pub fn controller_device_id(&self) -> &'static str {
        self.instance.controller_kind().manifest_device_id()
    }

    /// Start the XR preview on `session` and bind the graphics session: on
    /// Windows from wgpu's D3D12 device/queue (`gpu`), on Quest from the
    /// runtime-created Vulkan handles stashed by [`XrHost::bring_up_vulkan`].
    ///
    /// The HMD-sync camera behavior is authored on the XR player's `Camera`
    /// prim and attached by the session's authored-behavior path — it is not
    /// injected here.
    ///
    /// # Errors
    ///
    /// Returns a message when the GPU is not on the expected backend / the
    /// Vulkan bring-up was skipped, or the session/graphics binding fails.
    #[allow(unused_variables)]
    pub fn start(&mut self, session: &mut StudioSession, gpu: &DesktopGpu) -> Result<(), String> {
        session
            .start_xr_preview(&self.loader)
            .map_err(|e| format!("failed to start XR preview: {e}"))?;

        // Spawn the hand-gesture recogniser on the fresh preview so a pinch
        // drives the `Gesture/pinch/*` → `*_grab` actions (the pinch_grab
        // behavior). No-op host-side when hand tracking is unsupported.
        #[cfg(feature = "gesture-recognition")]
        {
            session.enable_gesture_recognition_default();
        }

        #[cfg(all(idtx_xr, windows))]
        {
            let (device, queue) = gpu.d3d12_device_queue_ptrs().ok_or_else(|| {
                "wgpu is not on the DX12 backend; cannot bind OpenXR session".to_owned()
            })?;
            session
                .start_xr_graphics_session(&mut self.instance, device, queue)
                .map_err(|e| format!("failed to bind D3D12 OpenXR session: {e}"))?;
        }
        #[cfg(all(idtx_xr, target_os = "android"))]
        {
            let h = self.vk.ok_or_else(|| {
                "Vulkan XR device was not brought up before start (call bring_up_vulkan)".to_owned()
            })?;
            session
                .start_xr_graphics_session_vk(
                    &mut self.instance,
                    h.instance,
                    h.physical_device,
                    h.device,
                    h.queue_family_index,
                    h.queue_index,
                )
                .map_err(|e| format!("failed to bind Vulkan OpenXR session: {e}"))?;
        }
        Ok(())
    }
}
