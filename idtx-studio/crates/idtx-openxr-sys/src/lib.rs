//! Narrow, unsafe `OpenXR` loader shim + runtime capability probe.
//!
//! This crate isolates the raw `openxr` loader behind a small safe surface
//! the rest of the stack depends on without touching raw XR types:
//!
//! * [`XrLoader`] — loads the runtime and probes it
//!   ([`XrLoader::available_runtimes`]).
//! * [`RuntimeInfo`] — the harvested runtime name + version.
//! * [`XrError`] — the failure surface.
//! * [`XrSession`] — an opaque session placeholder (construction is
//!   intentionally absent).
//!
//! # Feature gating
//!
//! * `pcvr` — the real loader (`openxr::Entry::load`). Off by default so a
//!   desktop build with no headset needs no `OpenXR` loader at all.
//! * `mock-xr` — a deterministic in-process runtime for tests/CI.
//! * With neither feature, [`XrLoader::try_load`] returns
//!   [`XrError::NoRuntime`] and the crate links nothing XR-related.
//!
//! When both `pcvr` and `mock-xr` are enabled, the real loader wins.

use thiserror::Error;

// Graphics-API backend seam. Pure selection + capability logic, always
// compiled (no XR deps) so the D3D12→Vulkan migration surface is testable here.
mod backend;
pub use backend::{GraphicsBackend, VulkanBackend, XrGraphicsApi, graphics_api_for};

// Real graphics-bound session. The frame loop, reference spaces,
// action set, and the `XrInstance` / `XrGraphicsSession<G>` types are
// graphics-API-agnostic and live in `session_common`; the D3D12 (`session`) and
// Vulkan (`session_vk`) backends add only the graphics binding, swapchain
// formats, and raw image accessors. `idtx_xr` = (pcvr && windows) || (quest &&
// android); see this crate's `build.rs`.
//
// `doc_markdown` is allowed: the module docs are dense with Windows/OpenXR
// identifiers (D3D12, DXGI, AddRef, …) that read fine unbackticked.
#[cfg(idtx_xr)]
#[allow(clippy::doc_markdown)]
mod session_common;
// D3D12 graphics specifics (Windows PCVR).
#[cfg(all(idtx_xr, windows))]
#[allow(clippy::doc_markdown)]
mod session;
// Vulkan graphics specifics (Quest/Android).
#[cfg(all(idtx_xr, target_os = "android"))]
#[allow(clippy::doc_markdown)]
mod session_vk;

#[cfg(all(idtx_xr, feature = "metrics"))]
pub use session_common::XrPacingMetrics;
#[cfg(idtx_xr)]
pub use session_common::{
    ControllerInput, LocatedHandJoint, LocatedPose, XrEye, XrFrame, XrInstance,
};
// Records the live Android VM + Activity for the Quest instance-create path.
#[cfg(all(idtx_xr, target_os = "android"))]
pub use session_vk::set_android_activity;

/// The active graphics backend's session type: `openxr::Session<D3D12>` on
/// Windows PCVR, `openxr::Session<Vulkan>` on Quest/Android. Downstream names
/// this param-free alias; only one backend compiles per build.
#[cfg(all(idtx_xr, windows))]
pub type XrGraphicsSession = session_common::XrGraphicsSession<openxr::d3d::D3D12>;
#[cfg(all(idtx_xr, target_os = "android"))]
pub type XrGraphicsSession = session_common::XrGraphicsSession<openxr::vulkan::Vulkan>;

// ── Windows runtime-DLL loader ────────────────────────────────────────────────
//
// `openxr::Entry::load()` calls `LoadLibraryExW("openxr_loader.dll")`, the
// Khronos reference loader.  Meta Quest Link does NOT ship that loader — it
// registers its own runtime DLL (`LibOVRRTImpl64_1.dll`) directly under
// `HKLM\SOFTWARE\Khronos\OpenXR\1\ActiveRuntime` → JSON manifest.
//
// When the standard load fails we fall back to:
//   1. Reading the registry to find the manifest path.
//   2. Parsing `library_path` from the manifest JSON.
//   3. `LoadLibraryW(full_path)` + `GetProcAddress("xrGetInstanceProcAddr")`.
//   4. `Entry::from_get_instance_proc_addr(fn_ptr)`.
//
// This bypasses the Khronos layer machinery (no API-validation layers), which
// is acceptable for a production preview path.

#[cfg(all(windows, any(feature = "pcvr", feature = "quest")))]
// FFI-dense module: `borrow_as_ptr` fires on every `&mut x` passed to a Win32
// call, and `doc_markdown` on the many Win32/OpenXR identifiers in the docs.
#[allow(clippy::borrow_as_ptr, clippy::doc_markdown)]
mod win_loader {
    use std::os::windows::ffi::OsStrExt as _;
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, GetLastError};
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW,
    };
    use windows_sys::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ, RegCloseKey, RegEnumValueW, RegOpenKeyExW,
        RegQueryValueExW,
    };

    /// Read a `REG_SZ` string value from `HKEY_LOCAL_MACHINE\<subkey>\<value>`.
    fn read_hklm_sz(subkey: &str, value: &str) -> Option<String> {
        let subkey_w: Vec<u16> = subkey.encode_utf16().chain(Some(0)).collect();
        let value_w: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        unsafe {
            // HKEY is *mut c_void in windows-sys 0.61.
            let mut hkey = std::ptr::null_mut();
            if RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                subkey_w.as_ptr(),
                0,
                KEY_READ,
                &mut hkey,
            ) != ERROR_SUCCESS
            {
                return None;
            }
            let mut kind = 0u32;
            let mut size = 0u32;
            // First call: determine required buffer size.
            RegQueryValueExW(
                hkey,
                value_w.as_ptr(),
                std::ptr::null(),
                &mut kind,
                std::ptr::null_mut(),
                &mut size,
            );
            if kind != REG_SZ || size < 2 {
                RegCloseKey(hkey);
                return None;
            }
            let mut buf = vec![0u8; size as usize];
            let rc = RegQueryValueExW(
                hkey,
                value_w.as_ptr(),
                std::ptr::null(),
                &mut kind,
                buf.as_mut_ptr(),
                &mut size,
            );
            RegCloseKey(hkey);
            if rc != ERROR_SUCCESS {
                return None;
            }
            // Bytes → UTF-16 words (little-endian), strip null terminator.
            let words: Vec<u16> = buf[..size as usize]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            let end = words.iter().position(|&w| w == 0).unwrap_or(words.len());
            String::from_utf16(&words[..end]).ok()
        }
    }

    /// Read the active `OpenXR` runtime manifest path from the registry.
    ///
    /// Tries both the native 64-bit view and the `WOW6432Node` (32-bit) view.
    /// Supports two registry layouts used by different runtimes:
    ///
    /// * `ActiveRuntime` (REG_SZ) — single value whose data is the manifest path
    ///   (standard Khronos loader layout).
    /// * `AvailableRuntimes` (subkey) — each value *name* under this subkey is a
    ///   manifest path; value data is an ignored REG_DWORD 0.  Meta Quest Link
    ///   uses this layout.
    pub(super) fn active_runtime_manifest() -> Option<PathBuf> {
        for base in &[
            "SOFTWARE\\Khronos\\OpenXR\\1",
            "SOFTWARE\\WOW6432Node\\Khronos\\OpenXR\\1",
        ] {
            if let Some(s) = read_hklm_sz(base, "ActiveRuntime") {
                return Some(PathBuf::from(s));
            }
            let avail = format!("{base}\\AvailableRuntimes");
            if let Some(s) = first_value_name_under_hklm(&avail) {
                return Some(PathBuf::from(s));
            }
        }
        None
    }

    /// Return the name of the first value under `HKLM\<subkey>`.
    ///
    /// The `AvailableRuntimes` layout stores manifest paths as value *names*
    /// (not value data), so we enumerate and return the first name.
    fn first_value_name_under_hklm(subkey: &str) -> Option<String> {
        let subkey_w: Vec<u16> = subkey.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let mut hkey = std::ptr::null_mut();
            if RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                subkey_w.as_ptr(),
                0,
                KEY_READ,
                &mut hkey,
            ) != ERROR_SUCCESS
            {
                return None;
            }
            // File paths fit in MAX_PATH (260) chars; 2048 is generous headroom.
            let mut name_buf = vec![0u16; 2048];
            let mut name_len: u32 = 2048;
            let rc = RegEnumValueW(
                hkey,
                0, // index 0 = first value
                name_buf.as_mut_ptr(),
                &mut name_len,
                std::ptr::null(),
                std::ptr::null_mut(), // type not needed
                std::ptr::null_mut(), // data not needed
                std::ptr::null_mut(), // data size not needed
            );
            RegCloseKey(hkey);
            if rc != ERROR_SUCCESS {
                return None;
            }
            // name_len is the character count, not including the null terminator.
            String::from_utf16(&name_buf[..name_len as usize]).ok()
        }
    }

    /// Extract the `library_path` field from an `OpenXR` runtime manifest JSON.
    ///
    /// The manifest format is minimal and stable; we use a substring scan to
    /// avoid pulling in a JSON parser.
    fn parse_library_path(json: &str) -> Option<String> {
        let key = "\"library_path\"";
        let pos = json.find(key)?;
        let after_key = &json[pos + key.len()..];
        let colon = after_key.find(':')? + 1;
        let rest = after_key[colon..].trim_start();
        if !rest.starts_with('"') {
            return None;
        }
        let inner = &rest[1..];
        let end = inner.find('"')?;
        // Unescape JSON `\\` → `\` for Windows paths.
        Some(inner[..end].replace("\\\\", "\\"))
    }

    /// Resolve the runtime DLL path from a manifest file.
    ///
    /// `library_path` may be relative; it is resolved against the manifest's
    /// own directory.
    pub(super) fn runtime_dll_from_manifest(manifest: &std::path::Path) -> Option<PathBuf> {
        let json = std::fs::read_to_string(manifest).ok()?;
        let lib = parse_library_path(&json)?;
        let lib_path = PathBuf::from(&lib);
        let raw = if lib_path.is_absolute() {
            lib_path
        } else {
            manifest.parent().map(|d| d.join(&lib_path))?
        };
        // Resolve `.` / `..` without `canonicalize` — that adds `\\?\` on
        // Windows which `LoadLibraryW` rejects.
        Some(normalize_path(&raw))
    }

    /// Collapse `.` and `..` in a path without touching the filesystem.
    fn normalize_path(p: &std::path::Path) -> PathBuf {
        use std::path::Component;
        let mut out = PathBuf::new();
        for c in p.components() {
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other),
            }
        }
        out
    }

    /// Load an `OpenXR` runtime DLL and negotiate its `xrGetInstanceProcAddr`.
    ///
    /// `OpenXR` *runtimes* do not export `xrGetInstanceProcAddr` directly — that
    /// is what the Khronos loader (`openxr_loader.dll`) synthesises.  Runtimes
    /// export `xrNegotiateLoaderRuntimeInterface`, which the loader calls to
    /// obtain the function table.  Meta Quest Link ships **no** Khronos loader
    /// (its manifest points straight at `LibOVRRTImpl64_1.dll`), so we perform
    /// that negotiation ourselves and hand the resulting proc-addr to
    /// [`openxr::Entry::from_get_instance_proc_addr`].
    ///
    /// Uses `LOAD_WITH_ALTERED_SEARCH_PATH` so the runtime's sibling DLLs are
    /// found in its own directory.  The loaded module is intentionally leaked —
    /// it must stay resident for the process lifetime.
    pub(super) fn negotiate_get_instance_proc_addr(
        dll_path: &std::path::Path,
    ) -> Result<openxr::sys::pfn::GetInstanceProcAddr, String> {
        // openxr-sys 0.14 made the `loader` module private and dropped the
        // `Xr`/`Fn` name prefixes; the negotiation structs/consts are re-exported
        // at `openxr::sys::*` and the fn-pointer type lives under `pfn`.
        use openxr::sys::pfn::NegotiateLoaderRuntimeInterface;
        use openxr::sys::{
            CURRENT_LOADER_RUNTIME_VERSION, NegotiateLoaderInfo, NegotiateRuntimeRequest,
        };

        if !dll_path.exists() {
            return Err(format!("DLL not found at {}", dll_path.display()));
        }

        let wide: Vec<u16> = dll_path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: wide is a valid null-terminated path.
        let module = unsafe {
            LoadLibraryExW(
                wide.as_ptr(),
                std::ptr::null_mut(),
                LOAD_WITH_ALTERED_SEARCH_PATH,
            )
        };
        if module.is_null() {
            // SAFETY: GetLastError has no preconditions.
            let err = unsafe { GetLastError() };
            return Err(format!(
                "LoadLibraryExW({}) failed with OS error {err}",
                dll_path.display()
            ));
        }

        let sym = b"xrNegotiateLoaderRuntimeInterface\0";
        // SAFETY: module is a valid HMODULE; sym is null-terminated.
        let raw = unsafe { GetProcAddress(module, sym.as_ptr()) }.ok_or_else(|| {
            format!(
                "{} exports no xrNegotiateLoaderRuntimeInterface",
                dll_path.display()
            )
        })?;
        // SAFETY: the symbol has the documented negotiation ABI.
        let negotiate: NegotiateLoaderRuntimeInterface = unsafe { std::mem::transmute(raw) };

        let loader_info = NegotiateLoaderInfo {
            struct_type: NegotiateLoaderInfo::TYPE,
            struct_version: NegotiateLoaderInfo::VERSION,
            struct_size: std::mem::size_of::<NegotiateLoaderInfo>(),
            // `CURRENT_LOADER_RUNTIME_VERSION` is `usize` in openxr-sys 0.14;
            // the interface-version fields are `u32`.
            min_interface_version: CURRENT_LOADER_RUNTIME_VERSION as u32,
            max_interface_version: CURRENT_LOADER_RUNTIME_VERSION as u32,
            // Mirror the Khronos loader: advertise a max that spans the whole
            // major-version-1 range (`XR_MAKE_VERSION(1, 0x3ff, 0xfff)`) so a
            // runtime newer than our compiled `CURRENT_API_VERSION` (e.g. an
            // Oculus 1.1.61 ICD against SDK 1.1.54) is still accepted.
            min_api_version: openxr::sys::Version::new(1, 0, 0),
            max_api_version: openxr::sys::Version::new(1, 0x3ff, 0xfff),
        };
        let mut request = NegotiateRuntimeRequest {
            struct_type: NegotiateRuntimeRequest::TYPE,
            struct_version: NegotiateRuntimeRequest::VERSION,
            struct_size: std::mem::size_of::<NegotiateRuntimeRequest>(),
            runtime_interface_version: 0,
            runtime_api_version: openxr::sys::Version::new(0, 0, 0),
            get_instance_proc_addr: None,
        };

        // SAFETY: both pointers are valid for the duration of the call.
        let result = unsafe { negotiate(&loader_info, &mut request) };
        if result != openxr::sys::Result::SUCCESS {
            return Err(format!(
                "xrNegotiateLoaderRuntimeInterface failed: {result:?}"
            ));
        }
        request
            .get_instance_proc_addr
            .ok_or_else(|| "runtime negotiation returned a null xrGetInstanceProcAddr".to_owned())
    }
}

/// Failure surface for loading / probing an `OpenXR` runtime.
#[derive(Debug, Error)]
pub enum XrError {
    /// No `OpenXR` runtime is available (no loader, or the crate was built
    /// without the `pcvr`/`mock-xr` features).
    #[error("no OpenXR runtime available")]
    NoRuntime,
    /// The `OpenXR` loader could not be loaded.
    #[error("OpenXR runtime failed to load: {0}")]
    RuntimeLoadFailed(String),
    /// Creating an `OpenXR` instance failed.
    #[error("failed to create OpenXR instance: {0}")]
    CreateInstanceFailed(String),
    /// A graphics binding could not be established for a session.
    #[error("graphics binding failed: {0}")]
    GraphicsBindingFailed(String),
    /// The runtime signalled the session was lost.
    #[error("OpenXR session lost")]
    SessionLost,
}

/// Identifying info for an `OpenXR` runtime, harvested from
/// `xrGetInstanceProperties`.
///
/// `version` is the runtime's version rendered `major.minor.patch` (the
/// packed `OpenXR` `XrVersion` formatted for display) rather than a raw
/// integer — human-readable and free of narrowing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInfo {
    /// The runtime's reported name (e.g. `"SteamVR/OpenXR"`).
    pub name: String,
    /// The runtime's version, `major.minor.patch`.
    pub version: String,
}

/// The kind of XR controller connected at session-init time, as classified
/// from `xrGetInstanceProperties` + `xrGetSystemProperties`.
///
/// Detected once at preview-launch time — controller switching during runtime
/// is not a supported scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum XrControllerKind {
    /// Meta Touch Plus controllers (Quest 3 / Quest 3S).
    MetaTouchPlus,
    /// Meta Touch Pro controllers (Quest Pro).
    MetaTouchPro,
    /// Meta Touch v3 / Oculus Touch (Quest 2 and older Meta headsets).
    MetaTouch,
    /// Valve Index controllers.
    ValveIndex,
    /// Unknown or unsupported device; the runtime capsule fallback applies.
    Generic,
}

impl XrControllerKind {
    /// The manifest device id used as the key in `run.toml`'s
    /// `[xr.controllers]` table and passed to
    /// `RuntimeManifest::resolve_controller_asset`.
    #[must_use]
    pub fn manifest_device_id(self) -> &'static str {
        match self {
            Self::MetaTouchPlus => "quest3",
            Self::MetaTouchPro => "questpro",
            Self::MetaTouch => "quest2",
            Self::ValveIndex => "valve_index",
            Self::Generic => "generic",
        }
    }

    /// Classify from `OpenXR` runtime and system name strings.
    ///
    /// `runtime_name` is from `xrGetInstanceProperties`; `system_name` and
    /// `vendor_id` are from `xrGetSystemProperties`. Matching is
    /// case-insensitive.
    #[must_use]
    pub fn classify(runtime_name: &str, system_name: &str, _vendor_id: u32) -> Self {
        let rn = runtime_name.to_lowercase();
        let sn = system_name.to_lowercase();

        // Meta runtimes advertise "Oculus" or "Meta" in the runtime name;
        // the system name carries the exact headset model.
        let is_meta = rn.contains("oculus")
            || rn.contains("meta")
            || sn.contains("oculus")
            || sn.contains("meta")
            || sn.contains("quest");

        if is_meta {
            if sn.contains("quest pro") {
                return Self::MetaTouchPro;
            }
            if sn.contains("quest 3s") || sn.contains("quest 3") || sn.contains("quest3") {
                return Self::MetaTouchPlus;
            }
            // Quest 2 or unidentified Meta device → Touch v3.
            return Self::MetaTouch;
        }

        // SteamVR: identify the underlying device from the system name, since
        // Air Link / Virtual Desktop route Meta headsets through SteamVR.
        if rn.contains("steamvr") {
            if sn.contains("index") {
                return Self::ValveIndex;
            }
            if sn.contains("quest pro") {
                return Self::MetaTouchPro;
            }
            if sn.contains("quest 3s") || sn.contains("quest 3") || sn.contains("quest3") {
                return Self::MetaTouchPlus;
            }
            if sn.contains("quest") || sn.contains("oculus") {
                return Self::MetaTouch;
            }
        }

        Self::Generic
    }
}

/// Which backend an [`XrLoader`] is driving.
enum Backend {
    #[cfg(any(feature = "pcvr", feature = "quest"))]
    Real(openxr::Entry),
    #[cfg(all(feature = "mock-xr", not(any(feature = "pcvr", feature = "quest"))))]
    Mock(RuntimeInfo),
    /// Built without a backend feature. Never constructed
    /// ([`XrLoader::try_load`] returns [`XrError::NoRuntime`] first), but
    /// present so the loader's methods type-check in this configuration.
    #[cfg(not(any(feature = "pcvr", feature = "quest", feature = "mock-xr")))]
    #[allow(dead_code)]
    Unavailable,
}

/// A loaded `OpenXR` runtime handle.
pub struct XrLoader {
    backend: Backend,
}

impl std::fmt::Debug for XrLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Don't require `openxr::Entry: Debug`; just tag the backend.
        let kind = match &self.backend {
            #[cfg(any(feature = "pcvr", feature = "quest"))]
            Backend::Real(_) => "real",
            #[cfg(all(feature = "mock-xr", not(any(feature = "pcvr", feature = "quest"))))]
            Backend::Mock(_) => "mock",
            #[cfg(not(any(feature = "pcvr", feature = "quest", feature = "mock-xr")))]
            Backend::Unavailable => "unavailable",
        };
        f.debug_struct("XrLoader").field("backend", &kind).finish()
    }
}

impl XrLoader {
    /// Attempt to load an `OpenXR` runtime.
    ///
    /// # Errors
    ///
    /// Returns [`XrError::NoRuntime`] when the crate was built without a
    /// backend feature, or [`XrError::RuntimeLoadFailed`] when the real
    /// loader is present but no runtime could be loaded.
    #[allow(clippy::missing_const_for_fn)]
    // real path is non-const
    // Each backend is a separate cfg block ending in `return`; exactly one
    // is compiled, so clippy sees a "needless" return that is in fact
    // required for the multi-config source to type-check.
    #[allow(clippy::needless_return)]
    pub fn try_load() -> Result<Self, XrError> {
        #[cfg(any(feature = "pcvr", feature = "quest"))]
        {
            // Try the standard Khronos-loader search first (works on SteamVR,
            // WMR, and any runtime that installs openxr_loader.dll to System32
            // or PATH).
            //
            // OpenXR 0.22 threads platform info through loader init and instance
            // creation. On Android the loader MUST be initialised with the
            // JavaVM + Activity (`xrInitializeLoaderKHR` /
            // `XR_KHR_loader_init_android`); in 0.22 that happens inside
            // `Entry::load` via `AndroidPlatformInfo::init_loader`, replacing the
            // old standalone `Entry::initialize_android_loader` call. The VM +
            // Activity come from `set_android_activity`, which `android_main`
            // calls before the event loop starts (see `runtime-core`). On every
            // other platform `()` is the no-op platform info.
            #[cfg(target_os = "android")]
            let standard = unsafe {
                match session_vk::android_platform_info() {
                    Some(pi) => openxr::Entry::load(&pi),
                    None => {
                        tracing::warn!(
                            "set_android_activity was not called before XR load; the \
                             OpenXR loader cannot be initialised with the Activity and \
                             the Quest session will likely stay stuck at IDLE"
                        );
                        openxr::Entry::load(&())
                    }
                }
            };
            #[cfg(not(target_os = "android"))]
            let standard = unsafe { openxr::Entry::load(&()) };

            // On Windows, Meta Quest Link does NOT ship the Khronos loader.
            // If the standard search failed, fall back to loading the runtime
            // DLL directly: read the registry manifest, parse library_path,
            // then LoadLibraryW + GetProcAddress("xrGetInstanceProcAddr").
            #[cfg(windows)]
            let entry = match standard {
                Ok(e) => e,
                Err(loader_err) => {
                    let manifest = win_loader::active_runtime_manifest().ok_or_else(|| {
                        XrError::RuntimeLoadFailed(format!(
                            "openxr_loader.dll not found and \
                                 HKLM\\...\\Khronos\\OpenXR\\1\\ActiveRuntime \
                                 registry key is absent \
                                 (original error: {loader_err:?})"
                        ))
                    })?;
                    let dll =
                        win_loader::runtime_dll_from_manifest(&manifest).ok_or_else(|| {
                            XrError::RuntimeLoadFailed(format!(
                                "could not resolve library_path from manifest \
                                 {} (original error: {loader_err:?})",
                                manifest.display()
                            ))
                        })?;
                    // Negotiate the runtime interface directly (Meta ships no
                    // Khronos loader), yielding xrGetInstanceProcAddr.
                    let get_proc =
                        win_loader::negotiate_get_instance_proc_addr(&dll).map_err(|e| {
                            XrError::RuntimeLoadFailed(format!(
                                "{e} (standard load: {loader_err:?})"
                            ))
                        })?;
                    // SAFETY: get_proc came from a successful runtime negotiation.
                    unsafe { openxr::Entry::from_get_instance_proc_addr(get_proc, &()) }
                        .map_err(|e| XrError::CreateInstanceFailed(format!("{e:?}")))?
                }
            };

            #[cfg(not(windows))]
            let entry = standard.map_err(|e| XrError::RuntimeLoadFailed(format!("{e:?}")))?;

            return Ok(Self {
                backend: Backend::Real(entry),
            });
        }
        #[cfg(all(feature = "mock-xr", not(any(feature = "pcvr", feature = "quest"))))]
        {
            return Ok(Self {
                backend: Backend::Mock(RuntimeInfo {
                    name: "IDTX Mock XR Runtime".to_owned(),
                    version: "1.0.0".to_owned(),
                }),
            });
        }
        #[cfg(not(any(feature = "pcvr", feature = "quest", feature = "mock-xr")))]
        {
            Err(XrError::NoRuntime)
        }
    }

    /// Build a loader backed by an explicit mock [`RuntimeInfo`] (tests).
    #[cfg(all(feature = "mock-xr", not(any(feature = "pcvr", feature = "quest"))))]
    #[must_use]
    pub fn mock(info: RuntimeInfo) -> Self {
        Self {
            backend: Backend::Mock(info),
        }
    }

    /// Probe the loaded runtime(s), returning their identifying info.
    ///
    /// `OpenXR` exposes a single active runtime per loader, so this returns
    /// at most one entry — modelled as a `Vec` for a stable API as the
    /// probe grows (e.g. enumerating API layers).
    ///
    /// # Errors
    ///
    /// [`XrError::CreateInstanceFailed`] when a bare probe instance cannot
    /// be created or queried (real path only).
    pub fn available_runtimes(&self) -> Result<Vec<RuntimeInfo>, XrError> {
        match &self.backend {
            #[cfg(any(feature = "pcvr", feature = "quest"))]
            Backend::Real(entry) => {
                let app_info = openxr::ApplicationInfo {
                    application_name: "IDTX Studio (probe)",
                    ..Default::default()
                };
                // A bare instance (no extensions) is enough to read the
                // runtime's properties; a graphics binding is only needed
                // at session creation. `()` platform info: a probe
                // instance drives no session, so no Android Activity binding.
                let instance = entry
                    .create_instance(&app_info, &openxr::ExtensionSet::default(), &[], &())
                    .map_err(|e| XrError::CreateInstanceFailed(format!("{e:?}")))?;
                let props = instance
                    .properties()
                    .map_err(|e| XrError::CreateInstanceFailed(format!("{e:?}")))?;
                Ok(vec![RuntimeInfo {
                    name: props.runtime_name,
                    version: props.runtime_version.to_string(),
                }])
            }
            #[cfg(all(feature = "mock-xr", not(any(feature = "pcvr", feature = "quest"))))]
            Backend::Mock(info) => Ok(vec![info.clone()]),
            #[cfg(not(any(feature = "pcvr", feature = "quest", feature = "mock-xr")))]
            Backend::Unavailable => Ok(Vec::new()),
        }
    }

    /// Create the persistent, graphics-capable `OpenXR` instance (D3D12) used
    /// for real-device rendering. Created once at startup and held
    /// for the process lifetime; its adapter LUID drives wgpu device creation.
    ///
    /// # Errors
    ///
    /// [`XrError::CreateInstanceFailed`] if the instance or system cannot be
    /// created (e.g. no HMD present).
    #[cfg(all(idtx_xr, windows))]
    pub fn create_instance(&self) -> Result<XrInstance, XrError> {
        match &self.backend {
            Backend::Real(entry) => XrInstance::create(entry),
        }
    }

    /// Create the persistent, Vulkan-capable `OpenXR` instance
    /// (`XR_KHR_vulkan_enable2`) used for real-device rendering on Quest/Android.
    /// The Vulkan parallel to [`Self::create_instance`]; its graphics
    /// requirements drive the runtime-blessed `VkInstance`/`VkDevice` creation
    /// (see `idtx-studio-session::xr_vulkan`).
    ///
    /// # Errors
    ///
    /// [`XrError::CreateInstanceFailed`] if the instance or system cannot be
    /// created (e.g. no HMD present).
    #[cfg(all(idtx_xr, target_os = "android"))]
    pub fn create_instance_vk(&self) -> Result<XrInstance, XrError> {
        match &self.backend {
            Backend::Real(entry) => XrInstance::create_vk(entry),
        }
    }

    /// Probe the connected HMD system and classify the controller kind.
    ///
    /// Creates a bare (non-graphics) `OpenXR` instance, resolves the HMD system
    /// via `xrGetSystem`, and reads `xrGetSystemProperties` to classify the
    /// connected device. Returns [`XrControllerKind::Generic`] when no HMD is
    /// present; the caller then falls back to the manifest's `"generic"` entry.
    ///
    /// Call once at preview-launch time — not per-frame. Controller switching
    /// during runtime is not a supported scenario.
    ///
    /// # Errors
    ///
    /// Returns [`XrError::CreateInstanceFailed`] when a probe instance cannot
    /// be created (real path only). A missing HMD is NOT an error; it returns
    /// `Ok(XrControllerKind::Generic)`.
    #[cfg(any(feature = "pcvr", feature = "quest"))]
    pub fn detect_controller_kind(&self) -> Result<XrControllerKind, XrError> {
        let Backend::Real(entry) = &self.backend;

        let app_info = openxr::ApplicationInfo {
            application_name: "IDTX Studio (probe)",
            ..Default::default()
        };
        // Bare instance — no graphics extension needed for system property probe.
        // `()` platform info: a probe instance drives no session.
        let instance = entry
            .create_instance(&app_info, &openxr::ExtensionSet::default(), &[], &())
            .map_err(|e| XrError::CreateInstanceFailed(format!("{e:?}")))?;

        let runtime_name = instance
            .properties()
            .map(|p| p.runtime_name)
            .unwrap_or_default();

        // No HMD present → no system → fall through to Generic.
        let Ok(system) = instance.system(openxr::FormFactor::HEAD_MOUNTED_DISPLAY) else {
            return Ok(XrControllerKind::Generic);
        };

        // Read system_name + vendor_id from xrGetSystemProperties.
        let mut sys_props = openxr::sys::SystemProperties {
            ty: openxr::sys::StructureType::SYSTEM_PROPERTIES,
            next: std::ptr::null_mut(),
            system_id: system,
            vendor_id: 0,
            system_name: [0; 256],
            graphics_properties: openxr::sys::SystemGraphicsProperties {
                max_swapchain_image_height: 0,
                max_swapchain_image_width: 0,
                max_layer_count: 0,
            },
            tracking_properties: openxr::sys::SystemTrackingProperties {
                orientation_tracking: openxr::sys::FALSE,
                position_tracking: openxr::sys::FALSE,
            },
        };
        // SAFETY: sys_props is fully initialised above; pointers are valid.
        //#[allow(clippy::borrow_as_ptr)]
        let result = unsafe {
            (instance.fp().get_system_properties)(instance.as_raw(), system, &raw mut sys_props)
        };

        #[allow(clippy::cast_sign_loss)]
        let (system_name, vendor_id) = if result == openxr::sys::Result::SUCCESS {
            // system_name is a C char array ([i8; 256]); convert to owned String.
            let bytes: Vec<u8> = sys_props
                .system_name
                .iter()
                .map(|&c| c as u8)
                .take_while(|&b| b != 0)
                .collect();
            (
                String::from_utf8_lossy(&bytes).into_owned(),
                sys_props.vendor_id,
            )
        } else {
            (String::new(), 0u32)
        };

        Ok(XrControllerKind::classify(
            &runtime_name,
            &system_name,
            vendor_id,
        ))
    }
}

/// An opaque `OpenXR` session handle.
///
/// Real, graphics-bound session creation (`openxr::Session<D3D12>` /
/// `<Vulkan>`) lives in the backend session modules; this placeholder exists so
/// downstream contracts can name the type. Construction is intentionally absent.
#[derive(Debug)]
#[non_exhaustive]
pub struct XrSession {}

#[cfg(all(test, feature = "mock-xr", not(feature = "pcvr")))]
mod tests {
    use super::*;

    #[test]
    fn mock_loader_probes_runtime() {
        let loader = XrLoader::try_load().expect("mock loads");
        let runtimes = loader.available_runtimes().expect("probe");
        assert_eq!(runtimes.len(), 1);
        assert_eq!(runtimes[0].name, "IDTX Mock XR Runtime");
    }

    #[test]
    fn mock_loader_with_injected_info() {
        let info = RuntimeInfo {
            name: "Injected".to_owned(),
            version: "2.1.0".to_owned(),
        };
        let loader = XrLoader::mock(info.clone());
        assert_eq!(loader.available_runtimes().unwrap(), vec![info]);
    }
}

#[cfg(all(test, not(any(feature = "pcvr", feature = "mock-xr"))))]
mod stub_tests {
    use super::*;

    #[test]
    fn no_backend_reports_no_runtime() {
        assert!(matches!(XrLoader::try_load(), Err(XrError::NoRuntime)));
    }
}

#[cfg(test)]
mod controller_kind_tests {
    use super::XrControllerKind;

    #[test]
    fn meta_quest_pro_classified_from_meta_runtime() {
        let kind = XrControllerKind::classify("Oculus", "Oculus Quest Pro", 0);
        assert_eq!(kind, XrControllerKind::MetaTouchPro);
        assert_eq!(kind.manifest_device_id(), "questpro");
    }

    #[test]
    fn meta_quest3_classified_from_meta_runtime() {
        let kind = XrControllerKind::classify("Oculus", "Oculus Quest 3", 0);
        assert_eq!(kind, XrControllerKind::MetaTouchPlus);
        assert_eq!(kind.manifest_device_id(), "quest3");
    }

    #[test]
    fn meta_quest3s_classified() {
        let kind = XrControllerKind::classify("Meta XR Runtime", "Meta Quest 3S", 0);
        assert_eq!(kind, XrControllerKind::MetaTouchPlus);
    }

    #[test]
    fn meta_quest2_classified() {
        let kind = XrControllerKind::classify("Oculus", "Oculus Quest 2", 0);
        assert_eq!(kind, XrControllerKind::MetaTouch);
        assert_eq!(kind.manifest_device_id(), "quest2");
    }

    #[test]
    fn steamvr_valve_index() {
        let kind = XrControllerKind::classify("SteamVR/OpenXR", "Valve Index", 0);
        assert_eq!(kind, XrControllerKind::ValveIndex);
        assert_eq!(kind.manifest_device_id(), "valve_index");
    }

    #[test]
    fn steamvr_with_quest_pro_via_air_link() {
        // Quest Pro connected via Air Link through SteamVR.
        let kind = XrControllerKind::classify("SteamVR/OpenXR", "Oculus Quest Pro", 0);
        assert_eq!(kind, XrControllerKind::MetaTouchPro);
    }

    #[test]
    fn steamvr_with_quest3_via_air_link() {
        let kind = XrControllerKind::classify("SteamVR/OpenXR", "Oculus Quest 3", 0);
        assert_eq!(kind, XrControllerKind::MetaTouchPlus);
    }

    #[test]
    fn unknown_runtime_falls_back_to_generic() {
        let kind = XrControllerKind::classify("Windows Mixed Reality", "HP Reverb G2", 0);
        assert_eq!(kind, XrControllerKind::Generic);
        assert_eq!(kind.manifest_device_id(), "generic");
    }

    #[test]
    fn empty_strings_fall_back_to_generic() {
        let kind = XrControllerKind::classify("", "", 0);
        assert_eq!(kind, XrControllerKind::Generic);
    }

    #[test]
    fn classification_is_case_insensitive() {
        let kind = XrControllerKind::classify("STEAMVR/OPENXR", "VALVE INDEX", 0);
        assert_eq!(kind, XrControllerKind::ValveIndex);
    }
}
