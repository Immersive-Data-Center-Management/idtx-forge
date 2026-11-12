//! Windowed standalone player: a winit [`ApplicationHandler`] that opens a
//! stage in a [`StudioSession`], runs behaviors + (optionally) an `OpenXR`
//! session, and presents the offscreen render to the swapchain.
//!
//! Renders **no editor chrome**: this crate never enables `idtx-render`'s
//! `editor-overlay` feature (so world-axis lines compile out) and never sets a
//! selection, so `render_one_frame` passes `selection_obb == None` and draws no
//! outline. The synthetic headlight, the synthetic/preview camera, and the XR
//! controller capsules all flow through the normal snapshot channel and are
//! kept automatically.

use std::sync::Arc;

use idtx_platform_desktop::DesktopGpu;
use idtx_studio_session::{RenderWorldUnitScale, StudioSession};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

use crate::RuntimeError;
use crate::config::RunConfig;
use crate::present::Presenter;

/// Trackpad pixel-delta divisor normalising [`MouseScrollDelta::PixelDelta`]
/// into the same "notch" magnitude a wheel's [`MouseScrollDelta::LineDelta`]
/// reports (≈ one line per detent), so a `Mouse/Scroll`-bound axis behaves
/// comparably on a mouse wheel and a trackpad.
const PIXELS_PER_SCROLL_LINE: f32 = 16.0;

/// Map a physical key to the input-map source string it feeds
/// (`Device/Control` grammar, e.g. `KeyCode::KeyW` → `"Key/W"`).
///
/// Physical (layout-independent) codes are used so WASD stays under the same
/// keys on non-QWERTY layouts. Only the keys the desktop player bindings need
/// are mapped; everything else returns `None` (no source fed). Kept as a free
/// function so the mapping is unit-testable without a window.
fn keycode_to_source(code: KeyCode) -> Option<&'static str> {
    Some(match code {
        KeyCode::KeyW => "Key/W",
        KeyCode::KeyA => "Key/A",
        KeyCode::KeyS => "Key/S",
        KeyCode::KeyD => "Key/D",
        KeyCode::Space => "Key/Space",
        KeyCode::ShiftLeft | KeyCode::ShiftRight => "Key/Shift",
        _ => return None,
    })
}

/// Map a mouse button to the input-map source string it feeds
/// (`Mouse/<Button>` grammar, e.g. `MouseButton::Left` → `"Mouse/Left"`).
///
/// The five standard buttons get named sources; any extra button uses
/// `Mouse/Button<N>` from winit's `MouseButton::Other(n)` code. Bind these in
/// `input_map.toml` as `button` actions exactly like keyboard / controller
/// sources — the router matches them through the same generic held-source
/// path. Kept a free function so the mapping is unit-testable without a window.
fn mousebutton_to_source(button: MouseButton) -> String {
    match button {
        MouseButton::Left => "Mouse/Left".to_owned(),
        MouseButton::Right => "Mouse/Right".to_owned(),
        MouseButton::Middle => "Mouse/Middle".to_owned(),
        MouseButton::Back => "Mouse/Back".to_owned(),
        MouseButton::Forward => "Mouse/Forward".to_owned(),
        MouseButton::Other(n) => format!("Mouse/Button{n}"),
    }
}

/// Build the anonymous session-layer USDA that injects the runtime-detected
/// controller device `Model` onto a packaged rig's pose Xforms
/// (`/World/Player/Controllers/{Left,Right}`), for `device` — the manifest
/// `[xr.controllers.<id>]` key returned by [`XrHost::controller_device_id`].
///
/// The packaged twin of `idtx-studio-shell::preview_assembly::xr_layer_text`:
/// resolves the device→asset (+ mount) from the discovered [`RuntimeManifest`]
/// (`run.toml`, into which the cook folds the project's `xr_controllers.toml`
/// sidecar), reads each asset's own upAxis/metersPerUnit to normalize it
/// into world space, and authors an `over`-only `Model` reference layer via
/// [`controller_model_over_layer`]. Returns `""` when the manifest resolves no
/// mesh for the device, in which case the runtime capsule fallback applies.
///
/// [`RuntimeManifest`]: idtx_runtime_manifest::RuntimeManifest
/// [`XrHost::controller_device_id`]: crate::xr_host::XrHost::controller_device_id
/// [`controller_model_over_layer`]: idtx_studio_session::xr_controller_prefab::controller_model_over_layer
#[cfg(idtx_xr)]
fn controller_injection_session_layer(cfg: &RunConfig, device: &str) -> String {
    use idtx_runtime_manifest::RuntimeManifest;
    use idtx_studio_session::read_controller_coordinate_settings;
    use idtx_studio_session::xr_controller_prefab::{
        ControllerAsset, CoordinateSettings, MountOffset, controller_model_over_layer,
    };

    // Manifest + asset search roots. On Android `RuntimeManifest::discover()`
    // falls back to a bare default (current_exe is `/system/bin`, so neither
    // `run.toml` nor an `assets/` dir sits next to it) and `asset_search_roots()`
    // never names the extracted payload — so the cook-staged controller meshes
    // never resolve and the controllers render as geometry-less prims. The player
    // already knows the payload: `run_android` set `cfg.asset_search_dirs` to the
    // extracted `payload/assets`. Load the payload's own `run.toml` (its sibling)
    // and resolve the device→mesh over that root instead. Desktop/PCVR keeps the
    // discover()+asset_search_roots() path, where current_exe is the real bundle.
    #[cfg(target_os = "android")]
    let (manifest, roots) = {
        let roots: Vec<std::path::PathBuf> = cfg.asset_search_dirs.clone();
        let manifest = roots
            .first()
            .and_then(|assets| assets.parent())
            .map(|payload| payload.join("run.toml"))
            .and_then(|p| RuntimeManifest::load(p).ok())
            .unwrap_or_default();
        (manifest, roots)
    };
    #[cfg(not(target_os = "android"))]
    let (manifest, roots) = {
        let _ = cfg;
        let manifest = RuntimeManifest::discover();
        let roots = manifest.asset_search_roots();
        (manifest, roots)
    };

    let Some(resolved) = manifest.resolve_controller_asset_in_roots(&roots, device) else {
        tracing::info!(
            "no controller mesh resolved for device \"{device}\"; capsule fallback applies"
        );
        return String::new();
    };
    let mount = resolved.mount.map(|m| MountOffset {
        rotate_xyz: m.rotate(),
        translate: m.translate(),
        scale: m.scale(),
    });
    let asset = |p: &std::path::Path| ControllerAsset {
        asset_path: p.display().to_string(),
        settings: read_controller_coordinate_settings(p),
        mount_offset: mount,
    };
    let left = asset(&resolved.left);
    let right = asset(&resolved.right);
    tracing::info!(
        "injecting XR controller meshes (device \"{}\"): left={} right={}",
        resolved.device,
        resolved.left.display(),
        resolved.right.display()
    );
    let body = controller_model_over_layer(Some(&left), Some(&right), CoordinateSettings::WORLD);
    if body.is_empty() {
        return String::new();
    }
    // `controller_model_over_layer` emits a bare `over "World" { … }` *block* (no
    // USDA cookie) so it can be composed into headered layers. Here the string is
    // handed straight to `SdfLayer::ImportFromString` as the stage's session
    // layer, which requires the `#usda 1.0` magic header to detect the format —
    // without it the text parser fails at `:1:1 … 'over "World"'` and the runtime
    // aborts with "session layer content is not valid USDA". Wrap it exactly as
    // the shell's preview-assembly path (`xr_layer_text`) wraps its player prefab.
    format!(
        "#usda 1.0\n(\n    doc = \"IDTX XR controller injection (session layer, generated)\"\n)\n\n{body}"
    )
}

/// Capture (`want = true`) or release (`want = false`) the mouse cursor for the
/// desktop player's mouse-look.
///
/// Capturing hides the cursor and confines it to the window so a hard mouse
/// sweep neither leaves a visible pointer over — nor delivers clicks to — any
/// window stacked behind the render surface. winit's grab support is
/// platform-split: `Locked` is backed on macOS / Wayland, `Confined` on
/// Windows / X11, so we try `Locked` and fall back to `Confined`. Raw look still
/// comes from `DeviceEvent::MouseMotion`, which both modes keep delivering.
/// Releasing restores the normal visible, free cursor.
fn apply_cursor_capture(window: &Window, want: bool) {
    if want {
        if window
            .set_cursor_grab(CursorGrabMode::Locked)
            .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined))
            .is_err()
        {
            tracing::debug!("cursor grab unsupported on this platform; cursor only hidden");
        }
        window.set_cursor_visible(false);
    } else {
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
    }
}

/// Live per-window state, created on `resumed`.
struct Live {
    #[cfg(feature = "metrics")]
    frame_metrics: idtx_session_orchestration::frame_metrics::FrameMetrics,
    window: Arc<Window>,
    gpu: DesktopGpu,
    /// The runtime's session set: the current level plus an optional resident
    /// transfer/mask session. A single-stage run holds a
    /// set with no transfer, so `active_session[_mut]()` is always the one level
    /// and the frame loop is byte-identical to the old single-`session` field.
    sessions: crate::SessionSet,
    /// The level-transition driver, present only for a multi-level experience
    /// (`cfg.levels` non-empty). Polled once per frame right after
    /// `tick_behaviors`; `None` on a single-stage run (never polled).
    driver: Option<crate::TransitionDriver>,
    presenter: Presenter,
    /// The [`SessionSet::active_epoch`] last reflected in the presenter's cached
    /// bind group. When it falls behind the set's current epoch — a transfer cut
    /// or a `set_current` swap handed a new `display_view` — the frame loop
    /// invalidates the presenter so it rebuilds against the new source view
    /// instead of blitting the dropped session's last frame.
    presented_epoch: u64,
    last_frame: std::time::Instant,
    /// Whether an XR session is actively driving the headset this run.
    xr_active: bool,
    /// The lifted `OpenXR` session (real-XR builds only), owned here — above the
    /// `StudioSession` — so a level transition that drops the active session
    /// cannot tear the headset down. Installed into the active session only
    /// across its own tick/render and taken back before `driver.poll` performs
    /// a swap; see [`idtx_studio_session::XrRuntimeSession`].
    #[cfg(idtx_xr)]
    xr: Option<idtx_studio_session::XrRuntimeSession>,
    /// The [`crate::SessionSet::active_epoch`] at which `xr` was last installed.
    /// When it differs on the next install the active session changed (a
    /// transfer cut or a level swap), so the controller proxies are re-seeded
    /// into the new session's world.
    #[cfg(idtx_xr)]
    xr_installed_epoch: u64,
    /// Rendered-frame counter, for the startup heartbeat log (confirms the
    /// frame loop is actually running on-device).
    frames: u64,
}

/// Install the runtime-owned XR session into the currently-active session so
/// its tick / `render_xr` path can drive the headset, re-seeding controller
/// proxies when the active session changed since the last install (a transfer
/// cut or a level swap). No-op when no XR session is owned (XR never started,
/// or a real headset exit cleared it).
#[cfg(idtx_xr)]
fn install_runtime_xr(live: &mut Live) {
    if let Some(xr) = live.xr.take() {
        let epoch = live.sessions.active_epoch();
        let reseed = epoch != live.xr_installed_epoch;
        live.sessions
            .active_session_mut()
            .install_xr_runtime(xr, reseed);
        live.xr_installed_epoch = epoch;
    }
}

/// Take the runtime-owned XR session back out of the active session — called
/// before `driver.poll` (which may drop that session) and after rendering, so
/// no `StudioSession` owns XR at the swap point.
#[cfg(idtx_xr)]
fn take_runtime_xr(live: &mut Live) {
    live.xr = live.sessions.active_session_mut().take_xr_runtime();
}

/// The windowed player application.
pub struct RuntimePlayer {
    cfg: RunConfig,
    live: Option<Live>,
    /// Set once the run has finished (window closed or fatal error), so the
    /// caller can surface an error after the loop returns.
    result: Result<(), RuntimeError>,
    /// Whether we currently *want* to hold the mouse cursor (hidden + confined)
    /// for desktop mouse-look. A packaged/fullscreen run starts captured; a
    /// windowed preview starts free and captures on first click. ESC clears it
    /// (preview) and focus loss releases the OS grab without clearing it, so
    /// focus regain restores capture. Only meaningful in desktop mode.
    capture_intent: bool,
    /// Whether the window has actually *consumed* the mouse right now (the grab
    /// was last applied, not merely wanted). Tracks the last value handed to
    /// [`Self::set_cursor_capture`], so it is `false` whenever the cursor is
    /// free — including the focus-lost window where `capture_intent` is still
    /// `true` but the OS grab has been released. In desktop mode this gates
    /// whether mouse input (motion, buttons, scroll) is forwarded to the
    /// session at all (see [`Self::forward_mouse_input`]); XR is never gated.
    cursor_captured: bool,
    /// Mouse-button input sources the window has forwarded as *held*
    /// (`Mouse/Left`, …) and not yet released. Tracked so that when the cursor
    /// is released mid-hold (ESC / focus loss while a button is down) we can
    /// release them, rather than leave a button stuck "pressed" in the session
    /// once its real button-up event is gated out. Motion/scroll need no such
    /// tracking — they are per-frame accumulators, not held sources.
    held_mouse_sources: std::collections::BTreeSet<String>,
}

impl RuntimePlayer {
    #[must_use]
    pub fn new(cfg: RunConfig) -> Self {
        // A packaged desktop (fullscreen) run captures from launch; a windowed
        // desktop preview waits for the first click. XR never captures (the
        // compositor owns input), so intent stays false there.
        let capture_intent =
            cfg.mode == crate::config::RunMode::Desktop && !cfg.is_headless() && cfg.fullscreen;
        Self {
            cfg,
            live: None,
            result: Ok(()),
            capture_intent,
            cursor_captured: false,
            held_mouse_sources: std::collections::BTreeSet::new(),
        }
    }

    /// Consume the player after the event loop exits, yielding any fatal error.
    pub fn into_result(self) -> Result<(), RuntimeError> {
        self.result
    }

    /// `true` when this run is a desktop window that drives mouse-look — the only
    /// configuration where cursor capture / fullscreen applies. XR (the mirror
    /// window belongs to the compositor) and headless runs are excluded.
    fn desktop_capture_mode(&self) -> bool {
        self.cfg.mode == crate::config::RunMode::Desktop && !self.cfg.is_headless()
    }

    /// Apply the current (or an explicit) capture state to the live window, and
    /// record it as the window's current mouse-consumption state. No-op on the
    /// window before it exists. Clones the `Arc<Window>` so the borrow of
    /// `self.live` ends before the call. Releasing the cursor (`want == false`)
    /// also releases any mouse buttons the window had forwarded as held, so a
    /// button still down at release does not stay stuck "pressed".
    fn set_cursor_capture(&mut self, want: bool) {
        self.cursor_captured = want;
        if let Some(window) = self.live.as_ref().map(|l| l.window.clone()) {
            apply_cursor_capture(&window, want);
        }
        if !want {
            self.release_held_mouse_buttons();
        }
    }

    /// Whether mouse input (motion, buttons, scroll) should currently be
    /// forwarded to the session. In desktop mode it is forwarded only while the
    /// window has consumed the cursor (grabbed it for mouse-look); a free cursor
    /// delivers no input, so clicks / moves / scrolls fall through to nothing
    /// until the user clicks back in. XR (PCVR / Quest / visionOS) and headless
    /// runs never gate — the compositor-driven path is left exactly as it was.
    fn forward_mouse_input(&self) -> bool {
        if self.desktop_capture_mode() {
            self.cursor_captured
        } else {
            true
        }
    }

    /// Release every mouse-button source the window forwarded as held and clear
    /// the tracking set. Called when the cursor is released (see
    /// [`Self::set_cursor_capture`]). No-op when nothing is held or no window is
    /// live yet.
    fn release_held_mouse_buttons(&mut self) {
        if self.held_mouse_sources.is_empty() {
            return;
        }
        if let Some(live) = self.live.as_mut() {
            let session = live.sessions.active_session_mut();
            for source in &self.held_mouse_sources {
                session.release_input(source);
            }
        }
        self.held_mouse_sources.clear();
    }

    fn init(&mut self, event_loop: &ActiveEventLoop) -> Result<Live, RuntimeError> {
        let mut attrs = Window::default_attributes()
            .with_title("IDTX Runtime")
            .with_inner_size(winit::dpi::PhysicalSize::new(
                self.cfg.viewport[0],
                self.cfg.viewport[1],
            ));
        // A packaged desktop player presents borderless-fullscreen. XR owns its
        // own presentation (the window is just a mirror), so never force it there.
        if self.desktop_capture_mode() && self.cfg.fullscreen {
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let window = Arc::new(event_loop.create_window(attrs)?);

        // On the real XR path the persistent instance must exist before the GPU
        // so wgpu can be pinned to the runtime-required DX12 adapter (LUID).
        #[cfg(idtx_xr)]
        let mut xr_host = if self.cfg.mode == crate::config::RunMode::Xr {
            tracing::info!("XR mode requested; probing for an OpenXR runtime");
            match crate::xr_host::XrHost::probe() {
                Some(host) => {
                    tracing::info!("OpenXR runtime detected; will start a headset session");
                    Some(host)
                }
                None => {
                    tracing::warn!(
                        "XR mode requested but no usable OpenXR runtime was detected \
                         (is a headset connected and its runtime active?); running desktop"
                    );
                    None
                }
            }
        } else {
            None
        };
        #[cfg(all(idtx_xr, windows))]
        let preferred_luid = xr_host
            .as_ref()
            .and_then(crate::xr_host::XrHost::preferred_luid);
        #[cfg(not(all(idtx_xr, windows)))]
        let preferred_luid: Option<(u32, i32)> = None;

        // Warn loudly if XR was asked for but this binary can't provide it.
        #[cfg(not(idtx_xr))]
        if self.cfg.mode == crate::config::RunMode::Xr {
            tracing::warn!(
                pcvr = cfg!(feature = "pcvr"),
                windows = cfg!(windows),
                "XR mode requested but this build has no real OpenXR support \
                 (requires the `pcvr` feature on a Windows target); running desktop"
            );
        }

        // GPU bring-up. On the Quest XR path the OpenXR runtime must create the
        // Vulkan device (it rejects an app-created one at `xrCreateSession`), so
        // bring it up through the host here and wrap it surfaceless; everything
        // else (desktop, Windows PCVR/D3D12) creates the device from the window.
        #[cfg(all(idtx_xr, target_os = "android"))]
        let gpu = {
            let size = window.inner_size();
            match xr_host
                .as_mut()
                .map(crate::xr_host::XrHost::bring_up_vulkan)
            {
                Some(Ok((device, queue))) => {
                    tracing::info!("Quest XR: OpenXR-dictated Vulkan device brought up");
                    DesktopGpu::from_wgpu(device, queue, size.width.max(1), size.height.max(1))
                }
                Some(Err(e)) => {
                    tracing::warn!("Quest XR Vulkan bring-up failed ({e}); running flat");
                    xr_host = None;
                    pollster::block_on(DesktopGpu::new(window.clone(), preferred_luid))?
                }
                None => pollster::block_on(DesktopGpu::new(window.clone(), preferred_luid))?,
            }
        };
        #[cfg(not(all(idtx_xr, target_os = "android")))]
        let gpu = pollster::block_on(DesktopGpu::new(window.clone(), preferred_luid))?;

        let size = window.inner_size();
        let viewport = [size.width.max(1), size.height.max(1)];

        // Packaged device-mesh injection. The packaged rig-embedded stage
        // carries the controller *pose* Xforms (`/World/Player/Controllers/
        // {Left,Right}`) but no `Model` — the concrete device is unknown until the
        // headset is on, so it cannot be baked at cook time. Now that the XR host
        // has probed the live system, resolve the connected device's mesh and
        // compose an anonymous session layer that authors the `Model` reference
        // `over` onto each pose Xform (the twin of the preview sublayer). Empty on
        // desktop, when no host probed, or when no mesh resolves (capsule
        // fallback). Absolute asset paths only — the anonymous session layer has
        // no directory to anchor a relative reference.
        //
        // Device selection prefers an explicit `--controller-device` (set by the
        // editor from its own first-hand OpenXR probe) over this process' live
        // probe. Over Quest Link the *second* process to query the runtime often
        // degrades to `"generic"`, so the passed-through id keeps the preview
        // injecting the right device meshes; without the flag (the packaged
        // path) we fall back to the local probe.
        #[cfg(idtx_xr)]
        let controller_session_layer = xr_host
            .as_ref()
            .map(|h| {
                let device = self
                    .cfg
                    .controller_device
                    .as_deref()
                    .unwrap_or_else(|| h.controller_device_id());
                if self.cfg.controller_device.is_some() {
                    tracing::info!(
                        device,
                        "injecting controller meshes for editor-supplied device (--controller-device)"
                    );
                } else {
                    tracing::info!(device, "injecting controller meshes for locally-probed device");
                }
                controller_injection_session_layer(&self.cfg, device)
            })
            .unwrap_or_default();
        #[cfg(not(idtx_xr))]
        let controller_session_layer = String::new();

        let mut session = StudioSession::open_with_gpu_and_session_layer(
            &self.cfg.stage,
            gpu.device.clone(),
            gpu.queue.clone(),
            viewport,
            self.cfg.texture_quality,
            RenderWorldUnitScale::METRE,
            &controller_session_layer,
        )?;

        // The packaged player renders no editor chrome. Disable overlays at
        // runtime as well as at compile time: `editor-overlay` can still be
        // compiled into the shared `idtx-render` rlib via cargo feature
        // unification when the workspace is built in a single invocation, so the
        // compile-time gate alone is not enough to keep world-axis lines out of a
        // preview frame.
        session.set_editor_overlays_enabled(false);

        // The session owns the single behavior host. The packaged
        // experience is always playing; attach authored runtime behaviors
        // (module bytes + manifest resolved relative to the stage directory).
        // Currently a no-op: USD behavior hydration is not wired; the XR sync
        // behaviors run through the same host once a preview starts.
        crate::configure_runtime_session(&mut session, &self.cfg);

        // Render through the local player's camera when the stage carries one
        // (net role `local`, e.g. the desktop/XR player rig's `/World/Player/
        // Camera`), so the runtime viewport is the player's eye rather than the
        // synthetic orbit camera. Falls back to the orbit camera when the stage
        // has no local-player camera. During an active XR preview the HMD-driven
        // preview camera takes precedence in `render_one_frame`; it rides the
        // same local-player camera, so the two paths stay consistent.
        if let Some(path) = session.local_player_camera_path() {
            tracing::info!("rendering through local player camera: {path}");
            session.use_scene_camera(path);
        }

        // Open the resident transfer/mask session and build
        // the transition driver, so the shipped windowed player (not just the
        // headless loop) can perform level transitions. Done **before** the XR
        // block below: on the XR path `gpu` is shadowed/moved (the mirror's
        // present mode is overridden), so the device/queue clones that the
        // transfer open and the driver require must be taken here while the
        // original `gpu` is still intact. A single-stage run declares no
        // `transfer_level` and no `levels`, so it opens no transfer session and
        // builds no driver — byte-identical.
        //
        // The current/level session above has already been fully configured and is
        // the one the XR session starts on; the transfer session is configured
        // identically via `configure_runtime_session` (the shared per-session
        // setup), with editor overlays forced off like the current session.
        let transfer = if let Some(transfer_path) = &self.cfg.transfer_level {
            let mut transfer = StudioSession::open_with_gpu_and_session_layer(
                transfer_path,
                gpu.device.clone(),
                gpu.queue.clone(),
                viewport,
                self.cfg.texture_quality,
                RenderWorldUnitScale::METRE,
                &controller_session_layer,
            )?;
            transfer.set_editor_overlays_enabled(false);
            crate::configure_runtime_session(&mut transfer, &self.cfg);
            Some(transfer)
        } else {
            None
        };
        let driver = (!self.cfg.levels.is_empty()).then(|| {
            crate::build_transition_driver(
                &self.cfg,
                gpu.device.clone(),
                gpu.queue.clone(),
                viewport,
                &controller_session_layer,
            )
        });

        // `xr_active` is only ever set true on the pcvr+windows path below.
        #[cfg_attr(not(idtx_xr), allow(unused_mut))]
        let mut xr_active = false;
        #[cfg(idtx_xr)]
        if let Some(host) = &mut xr_host {
            match host.start(&mut session, &gpu) {
                Ok(()) => {
                    xr_active = true;
                    tracing::info!(
                        "OpenXR session started; driving headset (desktop window mirrors left eye)"
                    );
                    // Render both controllers through the normal USD
                    // hydrate/extract path by default. The legacy bake is
                    // compiled out unless the
                    // `xr-bake` feature is enabled, so there is no opt-in env knob
                    // and no double-draw to guard against here.
                    #[cfg(feature = "authoring")]
                    {
                        if let Err(e) = session.set_controller_hydrated(true, true) {
                            tracing::warn!("enable hydrated left controller: {e}");
                        }
                        if let Err(e) = session.set_controller_hydrated(false, true) {
                            tracing::warn!("enable hydrated right controller: {e}");
                        }
                        tracing::info!("hydrated controllers enabled (left + right)");
                    }
                    // Headset paces frames via xrWaitFrame — drop desktop vsync
                    // so the mirror never throttles XR submission.
                    let mut gpu = gpu;
                    gpu.set_present_mode(wgpu::PresentMode::AutoNoVsync);
                    let presenter = Presenter::new(&gpu.device, gpu.configuration.format);
                    // The XR session was started on the current/level `session`;
                    // lift it out into runtime ownership so the per-level swap
                    // (which drops the active `StudioSession`) can no longer tear
                    // the headset down — it is re-installed into whichever session
                    // is active each frame. Desktop transitions are unaffected.
                    let mut sessions = crate::SessionSet::new(session, transfer);
                    let xr = sessions.current_mut().take_xr_runtime();
                    let xr_installed_epoch = sessions.active_epoch();
                    return Ok(Live {
                        #[cfg(feature = "metrics")]
                        frame_metrics: idtx_session_orchestration::frame_metrics::FrameMetrics::new("openxr", 11_111_111),
                        window,
                        gpu,
                        sessions,
                        driver,
                        presenter,
                        presented_epoch: u64::MAX,
                        last_frame: std::time::Instant::now(),
                        xr_active,
                        xr,
                        xr_installed_epoch,
                        frames: 0,
                    });
                }
                Err(e) => {
                    tracing::warn!("XR mode requested but unavailable ({e}); running desktop")
                }
            }
        }

        let presenter = Presenter::new(&gpu.device, gpu.configuration.format);
        let sessions = crate::SessionSet::new(session, transfer);
        Ok(Live {
            #[cfg(feature = "metrics")]
            frame_metrics: idtx_session_orchestration::frame_metrics::FrameMetrics::new("desktop", 16_666_667),
            window,
            gpu,
            sessions,
            driver,
            presenter,
            presented_epoch: u64::MAX,
            last_frame: std::time::Instant::now(),
            xr_active,
            #[cfg(idtx_xr)]
            xr: None,
            #[cfg(idtx_xr)]
            xr_installed_epoch: 0,
            frames: 0,
        })
    }

    fn render(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        #[cfg(feature = "metrics")]
        let _whole_frame = live.frame_metrics.start();
        #[cfg(feature = "metrics")]
        live.frame_metrics.configure(live.sessions.active_session().source_path(),
            [live.gpu.configuration.width, live.gpu.configuration.height], 1.0, live.gpu.device.features(),
            &format!("metrics remote={} openexec={} physics={}", cfg!(feature = "remote"), cfg!(feature = "openexec"), cfg!(feature = "physics")));
        // Startup heartbeat: confirms the frame loop is actually running on-device
        // (the first frames, then sparsely). If this logs repeatedly while the XR
        // session is stuck at IDLE, the loop is healthy and the runtime is
        // withholding READY; if it logs only once, the loop is not being driven.
        live.frames += 1;
        if live.frames <= 5 || live.frames % 300 == 0 {
            tracing::info!(
                frame = live.frames,
                xr_active = live.xr_active,
                "render tick"
            );
        }
        let now = std::time::Instant::now();
        let dt = now.saturating_duration_since(live.last_frame).as_secs_f32();
        live.last_frame = now;

        // Drain USD changes + advance animation.
        //
        // During an XR-active frame the reproject `tick()` inside
        // `author_hydrated_controller_poses` (called from `render_xr`) already
        // drains all pending changes — and it must run *after* the per-frame
        // controller pose is authored, so this explicit tick would only be a
        // redundant second drain. Skip it during XR; the desktop path keeps it
        // as its sole drain. (On a rare XR frame that skips rendering, the drain
        // is deferred one frame — harmless, nothing renders that frame anyway.)
        if !live.xr_active
            && let Err(e) = live.sessions.active_session_mut().tick() {
                tracing::warn!("session tick: {e}");
            }
        if let Err(e) = live.sessions.active_session_mut().tick_animation(f64::from(dt)) {
            tracing::warn!("animation tick: {e}");
        }

        // Single unified behavior tick: authored runtime behaviors plus,
        // when a preview is active, the XR HMD/controller pose feed → run_frame
        // → sweep — all in one `run_frame` per frame. No-op until hydration
        // lands and while no preview is active.
        //
        // Install the runtime-owned XR session into the active session for the
        // tick (`tick_behaviors` feeds the HMD/controller snapshot from it), then
        // take it back out before `driver.poll` below — which may drop the active
        // session on a swap. The session therefore never owns XR at the swap
        // point, so a transition can no longer tear the headset down.
        #[cfg(idtx_xr)]
        install_runtime_xr(live);
        live.sessions.active_session_mut().tick_behaviors(dt);
        #[cfg(idtx_xr)]
        take_runtime_xr(live);

        // Level-transition driver: service a behavior-posted transition
        // request right after `tick_behaviors` and before physics/exec/render, so
        // a request posted this frame is picked up without a one-frame lag (mirrors
        // `run_headless`). `driver` and `sessions` are disjoint `Live` fields, so
        // the two mutable borrows don't conflict. The active session may change
        // here (cut to the transfer/mask scene, or swap in the newly loaded level),
        // so the physics/exec/render calls below re-access `active_session_mut()`.
        // No driver on a single-stage run.
        //
        // XR: the OpenXR session is owned by the runtime (`live.xr`), not by any
        // `StudioSession`, and was taken back out just above — so this swap is
        // safe under an active headset. The transfer and swapped-in level sessions
        // get the XR re-installed (with a controller re-seed) just before
        // `render_xr` below.
        if let Some(driver) = live.driver.as_mut() {
            driver.poll(&mut live.sessions);
        }

        // Runtime physics (plan §6.1): step the `UsdPhysicsScene`-gated rigid-body
        // sim against this frame's kinematic goals (set by animation + behaviors
        // above) and author the moved dynamic set back into the session layer,
        // before the render reads the result. A no-op unless the stage carries a
        // `UsdPhysicsScene` prim; compiled only under the `physics` feature. On the
        // XR path we defer the reprojection: `render_xr` below drains the physics
        // writes together with the controller poses in a single pass.
        #[cfg(feature = "physics")]
        if let Err(e) = live.sessions.active_session_mut().tick_physics(dt, live.xr_active) {
            tracing::warn!("physics tick: {e}");
        }

        // Runtime OpenExec dataflow (plan §4): author REST/compute-node results
        // into the session layer. A no-op unless the stage carries an IDTX compute
        // graph. On the XR path defer reprojection so `render_xr` folds these
        // writes into its single drain (same as physics above).
        #[cfg(feature = "openexec")]
        if let Err(e) = live.sessions.active_session_mut().tick_exec(live.xr_active) {
            tracing::warn!("exec tick: {e}");
        }

        // XR: stereo render; mirror the left eye. The XR sync behaviors already
        // ran inside `tick_behaviors` above.
        #[cfg_attr(not(idtx_xr), allow(unused_mut))]
        let mut xr_mirrored = false;
        if live.xr_active {
            // Re-install the runtime-owned XR into whatever session is active
            // after `driver.poll` (the original level, the transfer/mask scene,
            // or a freshly-swapped level), re-seeding controller proxies into
            // that session's world if it changed.
            #[cfg(idtx_xr)]
            install_runtime_xr(live);
            #[cfg(idtx_xr)]
            match live.sessions.active_session_mut().render_xr(false) {
                Some(msg) => {
                    // A genuine headset exit (removed / system-menu exit / session
                    // lost). `stop_xr_preview` ends the installed session; drop our
                    // handle so it is not re-installed, and fall back to flat.
                    tracing::info!("OpenXR session ended: {msg}");
                    live.sessions.active_session_mut().stop_xr_preview();
                    live.xr = None;
                    live.xr_active = false;
                    let _ = live.gpu.set_present_mode(wgpu::PresentMode::AutoVsync);
                }
                None => {
                    // Frame submitted to the headset; take XR back out so the next
                    // `driver.poll` swap stays safe.
                    take_runtime_xr(live);
                    xr_mirrored = true;
                }
            }
        }

        if !xr_mirrored {
            live.sessions.active_session_mut().render_one_frame(false);
        }

        // Present the offscreen colour view to the window swapchain. Surfaceless
        // (Quest XR, where output already went to the XR swapchains in render_xr)
        // has no window swapchain to present to — skip.
        let Some(surface) = live.gpu.surface.as_ref() else {
            return;
        };
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                let size = live.window.inner_size();
                live.gpu.resize(size.width, size.height);
                live.presenter.invalidate();
                return;
            }
            wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Validation => return,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        // The active session may have changed this frame (a transfer cut or a
        // level swap in `driver.poll`), handing the presenter a different
        // `display_view` of the same size. The presenter caches its blit bind
        // group against the source view and rebuilds only when invalidated, so
        // without this it would keep blitting the dropped session's last frame
        // (a frozen screen). Invalidate on any active-session-epoch change.
        let epoch = live.sessions.active_epoch();
        if epoch != live.presented_epoch {
            live.presenter.invalidate();
            live.presented_epoch = epoch;
        }
        live.presenter.present(
            &live.gpu.device,
            &live.gpu.queue,
            live.sessions.active_session().display_view(),
            &view,
        );
        frame.present();
    }
}

impl ApplicationHandler for RuntimePlayer {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_some() {
            // Already live. On desktop winit resumes exactly once, so this is
            // never reached; on Android a resume after a `suspended` tear-down
            // sees `live == None` (see `suspended`) and re-initialises below.
            return;
        }
        // Android drives a continuous frame loop (immersive XR paces itself via
        // `xrWaitFrame`; the flat path animates behaviors/anim each tick), and the
        // occluded immersive window never delivers `RedrawRequested`, so the loop
        // must be `Poll`-driven. Desktop keeps the default `Wait` + redraw-event
        // pacing unchanged.
        #[cfg(target_os = "android")]
        event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);
        match self.init(event_loop) {
            Ok(live) => {
                live.window.request_redraw();
                self.live = Some(live);
                // A packaged/fullscreen desktop run grabs the cursor immediately
                // (capture_intent was seeded true in `new`); a windowed preview
                // starts free and captures on the first click.
                if self.desktop_capture_mode() && self.capture_intent {
                    self.set_cursor_capture(true);
                }
            }
            Err(e) => {
                self.result = Err(e);
                event_loop.exit();
            }
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        // On Android the native window (and the wgpu surface bound to it) is
        // destroyed when the activity is backgrounded; drop all live GPU +
        // surface + session state so the next `resumed` rebuilds it against the
        // fresh window. Desktop winit does not tear the surface down on suspend,
        // so this stays a no-op there (the live state is preserved across the
        // event, and desktop never issues a second `resumed`).
        #[cfg(target_os = "android")]
        {
            if let Some(mut live) = self.live.take() {
                tracing::info!("surface suspended; releasing GPU/session until resume");
                // Flush any rolling XR metrics on the way down (no-op flat).
                live.sessions.active_session_mut().stop_xr_preview();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(live) = self.live.as_mut() {
                    live.gpu.resize(size.width, size.height);
                    let vp = [size.width.max(1), size.height.max(1)];
                    // Resize BOTH resident sessions (the level and, if present, the
                    // transfer/mask), not just the active one, so the inactive one
                    // is not left at a stale size when the driver cuts to it; and
                    // update the driver so a level it loads after this resize opens
                    // at the current window size.
                    live.sessions.current_mut().resize_viewport(vp);
                    if let Some(transfer) = live.sessions.transfer_mut() {
                        transfer.resize_viewport(vp);
                    }
                    if let Some(driver) = live.driver.as_mut() {
                        driver.set_viewport(vp);
                    }
                    live.presenter.invalidate();
                    live.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::Focused(focused) => {
                // Diagnostic: the Meta Quest runtime gates the XR session
                // IDLE->READY transition on the activity gaining immersive window
                // focus. If this never logs `focused=true`, the app is not the
                // focused VR activity and the session will stay IDLE.
                tracing::info!(focused, "window focus changed");
                // Desktop capture follows focus: release the OS grab on focus
                // loss so Alt-Tab / Cmd-Tab works, but keep `capture_intent`, so
                // regaining focus restores capture for a run that was holding it.
                if self.desktop_capture_mode() {
                    self.set_cursor_capture(focused && self.capture_intent);
                }
            }
            WindowEvent::KeyboardInput {
                event: ref key_event,
                ..
            } => {
                use winit::event::ElementState;

                // ESC in desktop mode: a packaged/fullscreen player quits; a
                // windowed preview releases the cursor (back to a normal pointer
                // so the window's close button is reachable) without quitting.
                // Clicking back into a released preview re-captures.
                if key_event.state == ElementState::Pressed
                    && key_event.physical_key == PhysicalKey::Code(KeyCode::Escape)
                    && self.desktop_capture_mode()
                {
                    if self.cfg.fullscreen {
                        event_loop.exit();
                    } else {
                        self.capture_intent = false;
                        self.set_cursor_capture(false);
                    }
                    return;
                }

                // Feed WASD / bound keys to the session as physical input
                // sources (`Key/W`, …). Held sources persist across frames
                // until released; the router derives Pressed/Held/Released
                // edges. Behaviors only apply them while Playing.
                if let PhysicalKey::Code(code) = key_event.physical_key
                    && let Some(source) = keycode_to_source(code)
                        && let Some(live) = self.live.as_mut() {
                            match key_event.state {
                                ElementState::Pressed => live.sessions.active_session_mut().press_input(source),
                                ElementState::Released => live.sessions.active_session_mut().release_input(source),
                            }
                        }

                // Press H to cycle hand-visualisation mode.
                // Auto → ControllerOnly → HandSkeletonOnly → Auto
                if key_event.state == ElementState::Pressed {
                    use winit::keyboard::Key;
                    if (key_event.logical_key == Key::Character("h".into())
                        || key_event.logical_key == Key::Character("H".into()))
                        && let Some(live) = self.live.as_mut() {
                            use idtx_studio_session::XrHandVisualization;
                            let next = match live.sessions.active_session().hand_visualization() {
                                XrHandVisualization::Auto => XrHandVisualization::ControllerOnly,
                                XrHandVisualization::ControllerOnly => {
                                    XrHandVisualization::HandSkeletonOnly
                                }
                                XrHandVisualization::HandSkeletonOnly => XrHandVisualization::Auto,
                            };
                            live.sessions.active_session_mut().set_hand_visualization(next);
                            let label = match next {
                                XrHandVisualization::Auto => "Auto",
                                XrHandVisualization::ControllerOnly => "Controller Only",
                                XrHandVisualization::HandSkeletonOnly => "Hand Skeleton Only",
                            };
                            live.window
                                .set_title(&format!("IDTX Runtime — Hand: {label}"));
                            tracing::info!("hand visualisation set to {label}");
                        }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                use winit::event::ElementState;

                // Click-to-capture: in a windowed desktop preview the first left
                // click (re)captures the cursor instead of acting as a game
                // input, so the user keeps a normal pointer until they click in.
                // A packaged/fullscreen run is already captured, so this is skipped.
                if self.desktop_capture_mode()
                    && !self.cfg.fullscreen
                    && !self.capture_intent
                    && button == MouseButton::Left
                    && state == ElementState::Pressed
                {
                    self.capture_intent = true;
                    self.set_cursor_capture(true);
                    return;
                }

                // Desktop forwards mouse buttons only while the window has
                // consumed the cursor; a free pointer feeds nothing (the user
                // must click back in first). XR / headless are never gated.
                if !self.forward_mouse_input() {
                    return;
                }

                // Feed mouse buttons as digital input sources (`Mouse/Left`,
                // …), the same held-source channel as keys. Behaviors bind
                // them as `button` actions and the router derives the
                // Pressed/Held/Released edges. Track the held set so a button
                // still down when the cursor is released is not left stuck.
                if let Some(live) = self.live.as_mut() {
                    let source = mousebutton_to_source(button);
                    match state {
                        ElementState::Pressed => {
                            live.sessions
                                .active_session_mut()
                                .press_input(source.clone());
                            self.held_mouse_sources.insert(source);
                        }
                        ElementState::Released => {
                            live.sessions.active_session_mut().release_input(&source);
                            self.held_mouse_sources.remove(&source);
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Scroll is forwarded only while the window has consumed the
                // cursor (desktop); XR / headless unchanged.
                if !self.forward_mouse_input() {
                    return;
                }
                // Normalise the wheel/trackpad delta to a signed scalar "notch"
                // and feed the per-frame `Mouse/Scroll` source. Line deltas
                // pass through (1 line ≈ 1 notch); pixel deltas (trackpads) are
                // scaled to a comparable magnitude. Positive = scroll up / away
                // from the user. Bind as an `axis` action, or read the raw
                // magnitude via the SDK `scroll()` accessor.
                #[allow(clippy::cast_possible_truncation)]
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / PIXELS_PER_SCROLL_LINE,
                };
                if scroll != 0.0
                    && let Some(live) = self.live.as_mut()
                {
                    live.sessions.active_session_mut().add_scroll_input(scroll);
                }
            }
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        // Raw, unaccelerated mouse motion → the `Mouse/Delta` axis2 source that
        // drives yaw (player) + pitch (camera). Raw device deltas (not
        // window-relative cursor moves) are what mouse-look wants; they arrive
        // even when the cursor is at a screen edge.
        //
        // Forwarded only while the window has consumed the cursor (desktop).
        // When the cursor is free the window must not steer the view off stray
        // motion — including the focus-lost case, where `cursor_captured` is
        // `false` even though capture is still intended. XR / headless unchanged.
        if !self.forward_mouse_input() {
            return;
        }
        if let DeviceEvent::MouseMotion { delta } = event
            && let Some(live) = self.live.as_mut() {
                #[allow(clippy::cast_possible_truncation)]
                let d = [delta.0 as f32, delta.1 as f32];
                live.sessions.active_session_mut().add_mouse_input(d);
            }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // A player animates continuously (behaviors, animation, HMD pose), so
        // keep the loop warm. (Occlusion/minimise handling is a future refinement.)
        //
        // On Android in immersive XR the 2D window is occluded by the compositor,
        // so winit may never deliver `RedrawRequested` — which would stall the XR
        // frame loop (and the headset would show nothing). Drive `render()`
        // directly on that path instead; `xrWaitFrame` inside paces it to the
        // headset's cadence, so this does not busy-spin.
        #[cfg(target_os = "android")]
        if self.live.as_ref().is_some_and(|l| l.xr_active) {
            self.render();
            return;
        }
        if let Some(live) = self.live.as_ref() {
            live.window.request_redraw();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Tear the XR preview down cleanly on *every* exit path — window close,
        // init error, OS quit — not just the OpenXR session-ended branch in
        // `render`. `stop_xr_preview` flushes the rolling `[xr-metrics]` JSON
        // dump, which otherwise never gets written: a normal window close
        // reaches `WindowEvent::CloseRequested => event_loop.exit()` without
        // ever calling it. Idempotent and a no-op when no preview is active.
        if let Some(live) = self.live.as_mut() {
            live.sessions.active_session_mut().stop_xr_preview();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasd_maps_to_key_sources() {
        assert_eq!(keycode_to_source(KeyCode::KeyW), Some("Key/W"));
        assert_eq!(keycode_to_source(KeyCode::KeyA), Some("Key/A"));
        assert_eq!(keycode_to_source(KeyCode::KeyS), Some("Key/S"));
        assert_eq!(keycode_to_source(KeyCode::KeyD), Some("Key/D"));
    }

    #[test]
    fn source_strings_match_the_demo_input_map() {
        // The bindings in testfiles/projects/Demo1/input_map.toml use exactly
        // these source strings; a rename on either side must break this test.
        for src in [
            keycode_to_source(KeyCode::KeyW).unwrap(),
            keycode_to_source(KeyCode::KeyA).unwrap(),
            keycode_to_source(KeyCode::KeyS).unwrap(),
            keycode_to_source(KeyCode::KeyD).unwrap(),
        ] {
            assert!(
                src.starts_with("Key/"),
                "source `{src}` lost its device prefix"
            );
        }
    }

    #[test]
    fn unbound_keys_feed_nothing() {
        assert_eq!(keycode_to_source(KeyCode::KeyQ), None);
        assert_eq!(keycode_to_source(KeyCode::Enter), None);
    }

    #[test]
    fn mouse_buttons_map_to_mouse_sources() {
        assert_eq!(mousebutton_to_source(MouseButton::Left), "Mouse/Left");
        assert_eq!(mousebutton_to_source(MouseButton::Right), "Mouse/Right");
        assert_eq!(mousebutton_to_source(MouseButton::Middle), "Mouse/Middle");
        assert_eq!(mousebutton_to_source(MouseButton::Back), "Mouse/Back");
        assert_eq!(mousebutton_to_source(MouseButton::Forward), "Mouse/Forward");
        // Extra buttons fall back to a numbered source.
        assert_eq!(mousebutton_to_source(MouseButton::Other(7)), "Mouse/Button7");
    }

    #[test]
    fn mouse_sources_keep_their_device_prefix() {
        // The router + input maps key off the `Mouse/` device prefix; a rename
        // here must break this test.
        for button in [
            MouseButton::Left,
            MouseButton::Right,
            MouseButton::Middle,
            MouseButton::Other(3),
        ] {
            assert!(
                mousebutton_to_source(button).starts_with("Mouse/"),
                "mouse source lost its device prefix"
            );
        }
    }

    use crate::config::{RunConfig, RunMode};
    use std::path::PathBuf;

    fn player(mode: RunMode, fullscreen: bool, headless: bool) -> RuntimePlayer {
        let mut cfg = RunConfig::new(PathBuf::from("s.usda"), mode);
        cfg.fullscreen = fullscreen;
        if headless {
            cfg.frames = Some(10);
        }
        RuntimePlayer::new(cfg)
    }

    #[test]
    fn desktop_forwards_mouse_only_while_cursor_consumed() {
        // A windowed desktop preview starts with a free cursor: no mouse input
        // is forwarded until the window consumes it.
        let mut p = player(RunMode::Desktop, false, false);
        assert!(!p.forward_mouse_input(), "free cursor must forward no input");
        p.cursor_captured = true;
        assert!(p.forward_mouse_input(), "consumed cursor forwards input");
        // Releasing the cursor (ESC / focus loss) stops forwarding again.
        p.cursor_captured = false;
        assert!(!p.forward_mouse_input(), "released cursor forwards no input");
    }

    #[test]
    fn xr_modes_never_gate_mouse_input() {
        // PCVR / Quest / visionOS run in XR mode; their input handling must be
        // untouched — mouse input is forwarded regardless of cursor state.
        let mut p = player(RunMode::Xr, false, false);
        assert!(p.forward_mouse_input());
        p.cursor_captured = true;
        assert!(p.forward_mouse_input());
        p.cursor_captured = false;
        assert!(p.forward_mouse_input());
    }

    #[test]
    fn headless_desktop_is_not_gated() {
        // A headless (surfaceless, fixed-frame) run has no window to consume the
        // cursor; it is excluded from the gate so its behavior is unchanged.
        let p = player(RunMode::Desktop, false, true);
        assert!(p.forward_mouse_input());
    }
}
