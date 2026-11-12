//! Graphics-API backend seam for the XR session (scaffold + doc).
//!
//! The real graphics-bound session ([`session`](crate::session), Windows + `pcvr`)
//! is coupled to **D3D12** end-to-end: it is an `xr::Session<openxr::d3d::D3D12>`,
//! creates its swapchains with `DXGI_FORMAT_*`, and binds the wgpu device through
//! `wgpu-hal`'s D3D12 backend. That is correct for Windows/Meta today, but a Linux
//! PCVR target ([`CookTarget::pcvr_linux`](crate::backend) selects it) needs a
//! **Vulkan** session — SteamVR/Monado on Linux do not offer a D3D12 binding.
//!
//! This module is the *seam*, not the migration: it names the API choice
//! ([`XrGraphicsApi`]) and the coupling points a Vulkan implementation must supply
//! ([`GraphicsBackend`]), and selects the right API for a target. Rewriting
//! `session.rs` to be generic over the binding is deliberately deferred — it is
//! hardware-bound (needs a Vulkan headset + Linux/SteamVR to verify) and would
//! churn the one working XR path with no way to test the result here.
//!
//! ## What a Vulkan backend must provide (the coupling points in `session.rs`)
//!
//! 1. **Instance extension** — `ext.khr_vulkan_enable2 = true` instead of
//!    `khr_d3d12_enable`.
//! 2. **Adapter match** — `Vulkan::requirements()` yields the required
//!    `VkPhysicalDevice` (by UUID), replacing the DXGI-LUID adapter match wgpu
//!    uses to pick the same GPU the headset drives.
//! 3. **Session create info** — `SessionCreateInfoVulkan { instance, physical_device,
//!    device, queue_family_index, queue_index }` from the wgpu-hal Vulkan device,
//!    replacing `SessionCreateInfoD3D12 { device, queue }`.
//! 4. **Swapchain format** — a `VK_FORMAT_R8G8B8A8_SRGB` / `VK_FORMAT_D32_SFLOAT`
//!    pair instead of the `DXGI_FORMAT_*` constants.
//! 5. **Image import** — swapchain images arrive as `VkImage` handles; wrap them
//!    with `wgpu_hal::vulkan` texture import (the D3D12 path wraps `ID3D12Resource`).
//!
//! Everything above the binding — frame loop, view/projection math, action
//! handling, the depth-layer submission — is API-agnostic and moves unchanged.

/// The graphics API an XR session binds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrGraphicsApi {
    /// Direct3D 12 — the implemented Windows/Meta path.
    D3d12,
    /// Vulkan — the Linux/SteamVR + Monado path (scaffolded, see module docs).
    Vulkan,
}

impl XrGraphicsApi {
    /// The `OpenXR` instance extension name this API requires.
    #[must_use]
    pub const fn instance_extension(self) -> &'static str {
        match self {
            XrGraphicsApi::D3d12 => "XR_KHR_D3D12_enable",
            XrGraphicsApi::Vulkan => "XR_KHR_vulkan_enable2",
        }
    }

    /// Whether this crate currently implements a session for this API.
    #[must_use]
    pub const fn is_implemented(self) -> bool {
        matches!(self, XrGraphicsApi::D3d12)
    }
}

/// Select the graphics API for a Rust target triple.
///
/// Windows binds D3D12 (the implemented path); every other host that would run
/// PCVR (Linux) binds Vulkan. macOS has no PCVR runtime and is not a target here.
#[must_use]
pub fn graphics_api_for(triple: &str) -> XrGraphicsApi {
    if triple.contains("windows") {
        XrGraphicsApi::D3d12
    } else {
        XrGraphicsApi::Vulkan
    }
}

/// The seam a graphics backend fills to bind an `OpenXR` session (scaffold).
///
/// The D3D12 path in [`session`](crate::session) predates this trait and is not
/// yet re-expressed through it; the trait exists so the Vulkan work is a
/// *fill-in-the-impl*, and so the selection + capability logic is testable now.
pub trait GraphicsBackend {
    /// Which API this backend binds.
    fn api(&self) -> XrGraphicsApi;

    /// The swapchain color format token (API-specific numeric constant), for the
    /// documentation / manifest — not the live session yet.
    fn color_format(&self) -> u32;

    /// The swapchain depth format token.
    fn depth_format(&self) -> u32;
}

/// A Vulkan backend descriptor. **Scaffold:** it carries the format constants and
/// reports [`XrGraphicsApi::Vulkan`], but constructing a live Vulkan session is the
/// deferred work — see the module docs for the coupling points.
#[derive(Debug, Clone, Copy, Default)]
pub struct VulkanBackend;

impl GraphicsBackend for VulkanBackend {
    fn api(&self) -> XrGraphicsApi {
        XrGraphicsApi::Vulkan
    }
    /// `VK_FORMAT_R8G8B8A8_SRGB`.
    fn color_format(&self) -> u32 {
        43
    }
    /// `VK_FORMAT_D32_SFLOAT`.
    fn depth_format(&self) -> u32 {
        126
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_binds_d3d12_others_vulkan() {
        assert_eq!(
            graphics_api_for("x86_64-pc-windows-msvc"),
            XrGraphicsApi::D3d12
        );
        assert_eq!(
            graphics_api_for("x86_64-unknown-linux-gnu"),
            XrGraphicsApi::Vulkan
        );
        // Quest/Android standalone binds Vulkan — the same seam Linux PCVR needs.
        assert_eq!(
            graphics_api_for("aarch64-linux-android"),
            XrGraphicsApi::Vulkan
        );
    }

    #[test]
    fn only_d3d12_is_implemented_today() {
        assert!(XrGraphicsApi::D3d12.is_implemented());
        assert!(!XrGraphicsApi::Vulkan.is_implemented());
    }

    #[test]
    fn extension_names_match_api() {
        assert_eq!(
            XrGraphicsApi::D3d12.instance_extension(),
            "XR_KHR_D3D12_enable"
        );
        assert_eq!(
            XrGraphicsApi::Vulkan.instance_extension(),
            "XR_KHR_vulkan_enable2"
        );
    }

    #[test]
    fn vulkan_backend_reports_vulkan_and_formats() {
        let b = VulkanBackend;
        assert_eq!(b.api(), XrGraphicsApi::Vulkan);
        assert_eq!(b.color_format(), 43);
        assert_eq!(b.depth_format(), 126);
    }
}
