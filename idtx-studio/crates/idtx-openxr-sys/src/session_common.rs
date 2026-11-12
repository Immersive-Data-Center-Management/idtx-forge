//! Graphics-agnostic `OpenXR` session machinery shared by the D3D12 (Windows
//! PCVR) and Vulkan (Quest/Android) backends.
//!
//! The frame loop, reference spaces, swapchain sizing, controller action set,
//! and (optional) hand tracking are identical across graphics APIs — only the
//! graphics binding at session-create time, the swapchain pixel formats, and
//! the raw swapchain-image handle type differ. Those graphics specifics live in
//! `session.rs` (D3D12) and `session_vk.rs` (Vulkan); everything here is generic
//! over `G: openxr::Graphics`.
//!
//! The persistent instance ([`XrInstance`]) is non-generic — it holds the
//! graphics-API-agnostic `openxr::Instance` + `SystemId`; the per-backend
//! session-create methods ([`XrInstance::create_session`] on D3D12,
//! `create_session_vk` on Vulkan) each build the graphics binding, then call
//! the shared generic [`XrInstance::finish_session_setup`] to assemble the
//! swapchains + action set + the returned [`XrGraphicsSession`].
//!
//! # Not unit-testable
//!
//! Everything here needs a live runtime + headset; there are no unit tests.
//! The mock/headless seam stays in `idtx-platform-xr` (`mock-xr`).

use std::ffi::c_void;

use openxr as xr;

use crate::XrError;

/// A located pose crossing the openxr-sys boundary without a `glam` dependency.
/// `position` is metres in the reference space; `orientation` is a quaternion
/// `[x, y, z, w]`. The caller converts to its own math types.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocatedPose {
    pub position: [f32; 3],
    pub orientation: [f32; 4],
}

/// One controller's button / axis state for a frame, read from the per-frame
/// `OpenXR` action states and crossing the openxr-sys boundary without any
/// `openxr` types.
///
/// `primary` / `secondary` are the hand's lower / upper face buttons — `X`/`Y`
/// on the left controller, `A`/`B` on the right. `menu` is populated for the
/// left controller only (the Meta Touch right controller has no menu button).
#[derive(Debug, Clone, Copy, Default)]
// Each bool is an independent physical button; they are not a state machine.
#[allow(clippy::struct_excessive_bools)]
pub struct ControllerInput {
    /// Thumbstick deflection `[x, y]`, each in `[-1, 1]` (x right+, y up+).
    pub thumbstick: [f32; 2],
    /// Trigger pull, `[0, 1]`.
    pub trigger: f32,
    /// Grip / squeeze, `[0, 1]`.
    pub grip: f32,
    /// Thumbstick pressed in (click).
    pub thumbstick_click: bool,
    /// Lower face button — `X` (left) / `A` (right).
    pub primary: bool,
    /// Upper face button — `Y` (left) / `B` (right).
    pub secondary: bool,
    /// Menu button (left controller only).
    pub menu: bool,
}

/// One eye's data for a frame: pose, asymmetric field-of-view angles (radians,
/// `[left, right, up, down]`), and the acquired swapchain image index.
#[derive(Debug, Clone, Copy, Default)]
pub struct XrEye {
    pub pose: LocatedPose,
    pub fov: [f32; 4],
    pub image_index: u32,
    /// Acquired depth swapchain image index for this eye. Only meaningful when
    /// a depth composition layer is active (`XR_KHR_composition_layer_depth`);
    /// `0` and unused otherwise.
    pub depth_image_index: u32,
}

/// One located hand joint crossing the openxr-sys boundary.
///
/// Mirrors `XrHandJointLocationEXT` without the `openxr` crate's types.
/// `position` is metres in the reference space; `orientation` is `[x, y, z, w]`.
/// `radius` is the runtime's anatomical size estimate in metres.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocatedHandJoint {
    pub position: [f32; 3],
    pub orientation: [f32; 4], // xyzw
    pub radius: f32,
}

/// A begun frame ready for the caller to render both eyes into the swapchain
/// images identified by `eyes[i].image_index`.
///
/// Also carries the located controller grip poses for this frame (in STAGE
/// space). `None` per hand when the controller was not tracked this frame.
///
/// `left_hand` / `right_hand` carry 26-joint arrays when `XR_EXT_hand_tracking`
/// is supported and the hand was tracked this frame (requires `hand-tracking`
/// feature). Both are `None` when the feature is disabled or the hand is
/// untracked.
#[derive(Debug, Clone)]
pub struct XrFrame {
    pub eyes: [XrEye; 2],
    /// Left-controller grip pose in STAGE space, or `None` when untracked.
    pub left_controller: Option<LocatedPose>,
    /// Right-controller grip pose in STAGE space, or `None` when untracked.
    pub right_controller: Option<LocatedPose>,
    /// Left-controller aim pose in STAGE space, or `None` when untracked.
    pub left_aim: Option<LocatedPose>,
    /// Right-controller aim pose in STAGE space, or `None` when untracked.
    pub right_aim: Option<LocatedPose>,
    /// 26-joint left-hand pose, or `None` when untracked / unsupported.
    pub left_hand: Option<Box<[LocatedHandJoint; 26]>>,
    /// 26-joint right-hand pose, or `None` when untracked / unsupported.
    pub right_hand: Option<Box<[LocatedHandJoint; 26]>>,
    /// Left-controller button / axis state this frame (zeroed when untracked).
    pub left_input: ControllerInput,
    /// Right-controller button / axis state this frame (zeroed when untracked).
    pub right_input: ControllerInput,
}

/// Persistent `OpenXR` instance + resolved HMD system, created once at startup.
///
/// Graphics-API-agnostic: the D3D12 (`session.rs`) and Vulkan (`session_vk.rs`)
/// backends each add their own `impl XrInstance` blocks for the graphics
/// requirements probe and session-create path.
pub struct XrInstance {
    pub(crate) instance: xr::Instance,
    pub(crate) system: xr::SystemId,
    /// Whether `XR_KHR_composition_layer_depth` was advertised and enabled at
    /// instance creation. When `true`, the backend session-create methods create
    /// the per-eye depth swapchains and [`XrGraphicsSession::submit_frame`]
    /// chains a depth layer onto the projection.
    pub(crate) depth_layer_supported: bool,
}

impl std::fmt::Debug for XrInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XrInstance").finish_non_exhaustive()
    }
}

/// Per-eye swapchain resolution after applying the per-eye render-scale factor to
/// the runtime's recommended resolution.
///
/// The recommended resolution is the quality baseline, but on a mobile GPU
/// (Quest) rendering a heavy scene with 4× MSAA twice per frame at that
/// resolution is fragment-bound, and scaling it down is the single biggest,
/// cheapest win — it scales both shading and MSAA cost. The compositor upscales
/// the smaller image to the display, so geometry/UI stay correct and only
/// sharpness drops. The factor is `IDTX_XR_RENDER_SCALE` when set (clamped to
/// `[0.3, 1.0]`), else the platform default: **0.6 on Android/Quest** (QI-1), **1.0 on
/// PCVR**. Example: `IDTX_XR_RENDER_SCALE=0.5` ≈ cuts fragment load a further ~30 %.
fn scale_eye_resolution(width: u32, height: u32) -> (u32, u32) {
    // Android apps don't inherit a shell env, so the platform default — not the
    // var — is what actually applies on Quest; the var is for PCVR tuning.
    #[cfg(target_os = "android")]
    const DEFAULT_SCALE: f32 = 0.6;
    #[cfg(not(target_os = "android"))]
    const DEFAULT_SCALE: f32 = 1.0;

    let scale = std::env::var("IDTX_XR_RENDER_SCALE")
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .map(|s| s.clamp(0.3, 1.0))
        .unwrap_or(DEFAULT_SCALE);
    if (scale - 1.0).abs() < f32::EPSILON {
        return (width, height);
    }
    let scaled = |v: u32| ((v as f32 * scale).round() as u32).max(1);
    let (w, h) = (scaled(width), scaled(height));
    tracing::info!(
        recommended = ?(width, height),
        scaled = ?(w, h),
        scale,
        "applied IDTX_XR_RENDER_SCALE to per-eye XR swapchain resolution"
    );
    (w, h)
}

impl XrInstance {
    /// Classify the connected controller pair from this **already-open**
    /// persistent instance + resolved system.
    ///
    /// Reads the runtime name (`xrGetInstanceProperties`) and the system name /
    /// vendor id (`xrGetSystemProperties`) off `self.instance` / `self.system`
    /// and feeds them to [`crate::XrControllerKind::classify`]. Unlike
    /// [`crate::XrLoader::detect_controller_kind`] this does **not** create a
    /// second bare `OpenXR` instance — over Quest Link a second instance in the
    /// same process degrades its system properties and the classifier would see
    /// an empty system name and fall through to `Generic`. The packaged runtime
    /// therefore classifies from the one instance it already drives the headset
    /// with.
    ///
    /// The raw runtime/system strings and the resulting kind are logged at
    /// `info` so a mis-detection is diagnosable from the runtime console. Never
    /// fails — a failed property read degrades to `Generic` (the capsule/manifest
    /// fallback then applies).
    #[must_use]
    pub fn controller_kind(&self) -> crate::XrControllerKind {
        let runtime_name = self
            .instance
            .properties()
            .map(|p| p.runtime_name)
            .unwrap_or_default();

        let mut sys_props = xr::sys::SystemProperties {
            ty: xr::sys::StructureType::SYSTEM_PROPERTIES,
            next: std::ptr::null_mut(),
            system_id: self.system,
            vendor_id: 0,
            system_name: [0; 256],
            graphics_properties: xr::sys::SystemGraphicsProperties {
                max_swapchain_image_height: 0,
                max_swapchain_image_width: 0,
                max_layer_count: 0,
            },
            tracking_properties: xr::sys::SystemTrackingProperties {
                orientation_tracking: xr::sys::FALSE,
                position_tracking: xr::sys::FALSE,
            },
        };
        // SAFETY: sys_props is fully initialised above; pointers are valid for
        // the duration of the call.
        let result = unsafe {
            (self.instance.fp().get_system_properties)(
                self.instance.as_raw(),
                self.system,
                &mut sys_props,
            )
        };

        let (system_name, vendor_id) = if result == xr::sys::Result::SUCCESS {
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

        let kind = crate::XrControllerKind::classify(&runtime_name, &system_name, vendor_id);
        tracing::info!(
            runtime_name = %runtime_name,
            system_name = %system_name,
            vendor_id,
            ?kind,
            manifest_device = kind.manifest_device_id(),
            "classified XR controller kind from persistent instance"
        );
        kind
    }

    /// Assemble the graphics-bound [`XrGraphicsSession`] from an
    /// already-created `(session, waiter, stream)` triple: reference space,
    /// per-eye color (+ optional depth) swapchains at the runtime's recommended
    /// resolution, and the grip/aim controller action set.
    ///
    /// `color_format` / `depth_format` are the backend's swapchain pixel formats
    /// (`DXGI_FORMAT_*` on D3D12, `VK_FORMAT_*` on Vulkan). The rest is identical
    /// across graphics APIs, so both backend `create_session*` methods call this
    /// after building their graphics binding.
    pub(crate) fn finish_session_setup<G: xr::Graphics>(
        &self,
        session: xr::Session<G>,
        waiter: xr::FrameWaiter,
        stream: xr::FrameStream<G>,
        color_format: G::Format,
        depth_format: G::Format,
    ) -> Result<XrGraphicsSession<G>, XrError> {
        // STAGE (roomscale) reference space; fall back to LOCAL where STAGE is
        // unavailable (e.g. no guardian set up).
        let space = session
            .create_reference_space(xr::ReferenceSpaceType::STAGE, xr::sys::Posef::IDENTITY)
            .or_else(|_| {
                session
                    .create_reference_space(xr::ReferenceSpaceType::LOCAL, xr::sys::Posef::IDENTITY)
            })
            .map_err(|e| XrError::GraphicsBindingFailed(format!("reference space: {e:?}")))?;

        // Per-eye swapchains at the runtime's recommended resolution.
        let view_cfgs = self
            .instance
            .enumerate_view_configuration_views(
                self.system,
                xr::ViewConfigurationType::PRIMARY_STEREO,
            )
            .map_err(|e| XrError::GraphicsBindingFailed(format!("view config: {e:?}")))?;
        if view_cfgs.len() < 2 {
            return Err(XrError::GraphicsBindingFailed(
                "runtime reported fewer than 2 stereo views".to_owned(),
            ));
        }
        let width = view_cfgs[0].recommended_image_rect_width;
        let height = view_cfgs[0].recommended_image_rect_height;
        let (width, height) = scale_eye_resolution(width, height);
        let sc_info = xr::SwapchainCreateInfo::<G> {
            create_flags: xr::SwapchainCreateFlags::EMPTY,
            usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT
                | xr::SwapchainUsageFlags::TRANSFER_DST,
            format: color_format,
            sample_count: 1,
            width,
            height,
            face_count: 1,
            array_size: 1,
            mip_count: 1,
        };
        let sc0 = session
            .create_swapchain(&sc_info)
            .map_err(|e| XrError::GraphicsBindingFailed(format!("create_swapchain: {e:?}")))?;
        let sc1 = session
            .create_swapchain(&sc_info)
            .map_err(|e| XrError::GraphicsBindingFailed(format!("create_swapchain: {e:?}")))?;

        // Per-eye depth swapchains for `XR_KHR_composition_layer_depth`, created
        // only when the runtime advertised the extension. `TRANSFER_DST` so the
        // resolved single-sample depth can be copied in;
        // `DEPTH_STENCIL_ATTACHMENT` because the compositor consumes it as depth.
        let depth_swapchains = if self.depth_layer_supported {
            let depth_info = xr::SwapchainCreateInfo::<G> {
                create_flags: xr::SwapchainCreateFlags::EMPTY,
                usage_flags: xr::SwapchainUsageFlags::DEPTH_STENCIL_ATTACHMENT
                    | xr::SwapchainUsageFlags::TRANSFER_DST,
                format: depth_format,
                sample_count: 1,
                width,
                height,
                face_count: 1,
                array_size: 1,
                mip_count: 1,
            };
            let d0 = session.create_swapchain(&depth_info).map_err(|e| {
                XrError::GraphicsBindingFailed(format!("create depth swapchain: {e:?}"))
            })?;
            let d1 = session.create_swapchain(&depth_info).map_err(|e| {
                XrError::GraphicsBindingFailed(format!("create depth swapchain: {e:?}"))
            })?;
            Some([d0, d1])
        } else {
            None
        };

        // ── Controller action set ─────────────────────────────────────────────
        // Create a grip-pose action set once; attach it to the session so the
        // runtime can route the bindings to the connected controller profile.
        let action_set = self
            .instance
            .create_action_set("idtx_preview", "IDTX Preview", 0)
            .map_err(|e| XrError::GraphicsBindingFailed(format!("create_action_set: {e:?}")))?;

        let left_grip_action = action_set
            .create_action::<xr::Posef>("left_grip", "Left Grip Pose", &[])
            .map_err(|e| XrError::GraphicsBindingFailed(format!("left_grip action: {e:?}")))?;
        let right_grip_action = action_set
            .create_action::<xr::Posef>("right_grip", "Right Grip Pose", &[])
            .map_err(|e| XrError::GraphicsBindingFailed(format!("right_grip action: {e:?}")))?;

        // Aim-pose actions: the ergonomically-tilted "where the controller points"
        // pose (distinct from grip), used to drive the pointer ray.
        let left_aim_action = action_set
            .create_action::<xr::Posef>("left_aim", "Left Aim Pose", &[])
            .map_err(|e| XrError::GraphicsBindingFailed(format!("left_aim action: {e:?}")))?;
        let right_aim_action = action_set
            .create_action::<xr::Posef>("right_aim", "Right Aim Pose", &[])
            .map_err(|e| XrError::GraphicsBindingFailed(format!("right_aim action: {e:?}")))?;

        // Controller button / axis actions (thumbsticks, triggers, grips, face
        // buttons). Separate per-hand actions mirror the per-hand pose actions
        // above; a `create_action` failure is fatal (the action set is broken).
        let input_actions = ControllerInputActions {
            left_thumbstick: action_set
                .create_action::<xr::Vector2f>("left_thumbstick", "Left Thumbstick", &[])
                .map_err(|e| XrError::GraphicsBindingFailed(format!("left_thumbstick: {e:?}")))?,
            right_thumbstick: action_set
                .create_action::<xr::Vector2f>("right_thumbstick", "Right Thumbstick", &[])
                .map_err(|e| XrError::GraphicsBindingFailed(format!("right_thumbstick: {e:?}")))?,
            left_trigger: action_set
                .create_action::<f32>("left_trigger", "Left Trigger", &[])
                .map_err(|e| XrError::GraphicsBindingFailed(format!("left_trigger: {e:?}")))?,
            right_trigger: action_set
                .create_action::<f32>("right_trigger", "Right Trigger", &[])
                .map_err(|e| XrError::GraphicsBindingFailed(format!("right_trigger: {e:?}")))?,
            left_squeeze: action_set
                .create_action::<f32>("left_squeeze", "Left Grip", &[])
                .map_err(|e| XrError::GraphicsBindingFailed(format!("left_squeeze: {e:?}")))?,
            right_squeeze: action_set
                .create_action::<f32>("right_squeeze", "Right Grip", &[])
                .map_err(|e| XrError::GraphicsBindingFailed(format!("right_squeeze: {e:?}")))?,
            left_x: action_set
                .create_action::<bool>("left_x", "Left X Button", &[])
                .map_err(|e| XrError::GraphicsBindingFailed(format!("left_x: {e:?}")))?,
            right_a: action_set
                .create_action::<bool>("right_a", "Right A Button", &[])
                .map_err(|e| XrError::GraphicsBindingFailed(format!("right_a: {e:?}")))?,
            left_y: action_set
                .create_action::<bool>("left_y", "Left Y Button", &[])
                .map_err(|e| XrError::GraphicsBindingFailed(format!("left_y: {e:?}")))?,
            right_b: action_set
                .create_action::<bool>("right_b", "Right B Button", &[])
                .map_err(|e| XrError::GraphicsBindingFailed(format!("right_b: {e:?}")))?,
            left_thumbstick_click: action_set
                .create_action::<bool>("left_stick_click", "Left Thumbstick Click", &[])
                .map_err(|e| XrError::GraphicsBindingFailed(format!("left_stick_click: {e:?}")))?,
            right_thumbstick_click: action_set
                .create_action::<bool>("right_stick_click", "Right Thumbstick Click", &[])
                .map_err(|e| XrError::GraphicsBindingFailed(format!("right_stick_click: {e:?}")))?,
            left_menu: action_set
                .create_action::<bool>("left_menu", "Menu", &[])
                .map_err(|e| XrError::GraphicsBindingFailed(format!("left_menu: {e:?}")))?,
        };

        // Suggest bindings per interaction profile. `xrSuggestInteractionProfile
        // Bindings` *replaces* all bindings for a profile in one call, so every
        // binding for a profile — poses and inputs — must go in a single slice.
        //
        // The simple-controller profile has no thumbstick/face buttons, so it
        // binds poses only. The Meta Touch profile (`oculus/touch_controller`,
        // shared by Quest 2/3/Pro) binds poses *and* the full input set. Failures
        // here are non-fatal: the runtime may not support a given profile.
        let path = |s: &str| self.instance.string_to_path(s).ok();

        // Pose-only: simple controller.
        if let (Some(profile), Some(lgp), Some(rgp), Some(lap), Some(rap)) = (
            path("/interaction_profiles/khr/simple_controller"),
            path("/user/hand/left/input/grip/pose"),
            path("/user/hand/right/input/grip/pose"),
            path("/user/hand/left/input/aim/pose"),
            path("/user/hand/right/input/aim/pose"),
        ) {
            let _ = self.instance.suggest_interaction_profile_bindings(
                profile,
                &[
                    xr::Binding::new(&left_grip_action, lgp),
                    xr::Binding::new(&right_grip_action, rgp),
                    xr::Binding::new(&left_aim_action, lap),
                    xr::Binding::new(&right_aim_action, rap),
                ],
            );
        }

        // Poses + full input set: Meta Touch (Quest 2/3/Pro). Only bindings whose
        // component path resolves are pushed, so a profile missing a component
        // simply omits it rather than failing the whole suggestion.
        if let Some(profile) = path("/interaction_profiles/oculus/touch_controller") {
            let mut bindings: Vec<xr::Binding<'_>> = Vec::new();
            // Poses.
            if let Some(p) = path("/user/hand/left/input/grip/pose") {
                bindings.push(xr::Binding::new(&left_grip_action, p));
            }
            if let Some(p) = path("/user/hand/right/input/grip/pose") {
                bindings.push(xr::Binding::new(&right_grip_action, p));
            }
            if let Some(p) = path("/user/hand/left/input/aim/pose") {
                bindings.push(xr::Binding::new(&left_aim_action, p));
            }
            if let Some(p) = path("/user/hand/right/input/aim/pose") {
                bindings.push(xr::Binding::new(&right_aim_action, p));
            }
            // Analogue axes.
            if let Some(p) = path("/user/hand/left/input/trigger/value") {
                bindings.push(xr::Binding::new(&input_actions.left_trigger, p));
            }
            if let Some(p) = path("/user/hand/right/input/trigger/value") {
                bindings.push(xr::Binding::new(&input_actions.right_trigger, p));
            }
            if let Some(p) = path("/user/hand/left/input/squeeze/value") {
                bindings.push(xr::Binding::new(&input_actions.left_squeeze, p));
            }
            if let Some(p) = path("/user/hand/right/input/squeeze/value") {
                bindings.push(xr::Binding::new(&input_actions.right_squeeze, p));
            }
            // Thumbstick vectors.
            if let Some(p) = path("/user/hand/left/input/thumbstick") {
                bindings.push(xr::Binding::new(&input_actions.left_thumbstick, p));
            }
            if let Some(p) = path("/user/hand/right/input/thumbstick") {
                bindings.push(xr::Binding::new(&input_actions.right_thumbstick, p));
            }
            // Digital buttons.
            if let Some(p) = path("/user/hand/left/input/x/click") {
                bindings.push(xr::Binding::new(&input_actions.left_x, p));
            }
            if let Some(p) = path("/user/hand/left/input/y/click") {
                bindings.push(xr::Binding::new(&input_actions.left_y, p));
            }
            if let Some(p) = path("/user/hand/right/input/a/click") {
                bindings.push(xr::Binding::new(&input_actions.right_a, p));
            }
            if let Some(p) = path("/user/hand/right/input/b/click") {
                bindings.push(xr::Binding::new(&input_actions.right_b, p));
            }
            if let Some(p) = path("/user/hand/left/input/thumbstick/click") {
                bindings.push(xr::Binding::new(&input_actions.left_thumbstick_click, p));
            }
            if let Some(p) = path("/user/hand/right/input/thumbstick/click") {
                bindings.push(xr::Binding::new(&input_actions.right_thumbstick_click, p));
            }
            if let Some(p) = path("/user/hand/left/input/menu/click") {
                bindings.push(xr::Binding::new(&input_actions.left_menu, p));
            }

            let _ = self
                .instance
                .suggest_interaction_profile_bindings(profile, &bindings);
        }

        session
            .attach_action_sets(&[&action_set])
            .map_err(|e| XrError::GraphicsBindingFailed(format!("attach_action_sets: {e:?}")))?;

        let left_grip_space = left_grip_action
            .create_space(&session, xr::Path::NULL, xr::sys::Posef::IDENTITY)
            .map_err(|e| XrError::GraphicsBindingFailed(format!("left grip space: {e:?}")))?;
        let right_grip_space = right_grip_action
            .create_space(&session, xr::Path::NULL, xr::sys::Posef::IDENTITY)
            .map_err(|e| XrError::GraphicsBindingFailed(format!("right grip space: {e:?}")))?;
        let left_aim_space = left_aim_action
            .create_space(&session, xr::Path::NULL, xr::sys::Posef::IDENTITY)
            .map_err(|e| XrError::GraphicsBindingFailed(format!("left aim space: {e:?}")))?;
        let right_aim_space = right_aim_action
            .create_space(&session, xr::Path::NULL, xr::sys::Posef::IDENTITY)
            .map_err(|e| XrError::GraphicsBindingFailed(format!("right aim space: {e:?}")))?;

        Ok(XrGraphicsSession {
            instance: self.instance.clone(),
            session,
            waiter,
            stream,
            space,
            swapchains: [sc0, sc1],
            depth_swapchains,
            swap_width: width,
            swap_height: height,
            state: xr::SessionState::IDLE,
            running: false,
            frame_time: xr::Time::from_nanos(0),
            frame_views: Vec::new(),
            should_render: false,
            action_set,
            left_grip_action,
            right_grip_action,
            left_aim_action,
            right_aim_action,
            left_grip_space,
            right_grip_space,
            left_aim_space,
            right_aim_space,
            input_actions,
            #[cfg(feature = "hand-tracking")]
            left_hand_tracker: None,
            #[cfg(feature = "hand-tracking")]
            right_hand_tracker: None,
            #[cfg(feature = "hand-tracking")]
            pfn_locate_hand_joints: None,
            #[cfg(feature = "metrics")]
            pacing: XrPacingMetrics::default(),
            #[cfg(feature = "metrics")]
            last_begin_instant: None,
        })
    }

    /// Returns `true` when `XR_EXT_hand_tracking` is available on this system
    /// and the hand trackers were successfully created.
    ///
    /// Call once after the backend `create_session*`, before the first frame. On
    /// success the session's `left/right_hand_tracker` fields are populated and
    /// per-frame joint location is active.
    #[cfg(feature = "hand-tracking")]
    pub fn try_init_hand_tracking<G: xr::Graphics>(
        &mut self,
        session: &mut XrGraphicsSession<G>,
    ) -> bool {
        // Check system support.
        let mut ht_props = xr::sys::SystemHandTrackingPropertiesEXT {
            ty: xr::sys::StructureType::SYSTEM_HAND_TRACKING_PROPERTIES_EXT,
            next: std::ptr::null_mut(),
            supports_hand_tracking: xr::sys::FALSE,
        };
        let mut sys_props = xr::sys::SystemProperties {
            ty: xr::sys::StructureType::SYSTEM_PROPERTIES,
            next: (&mut ht_props as *mut xr::sys::SystemHandTrackingPropertiesEXT).cast(),
            system_id: self.system,
            vendor_id: 0,
            system_name: [0; 256],
            graphics_properties: xr::sys::SystemGraphicsProperties {
                max_swapchain_image_height: 0,
                max_swapchain_image_width: 0,
                max_layer_count: 0,
            },
            tracking_properties: xr::sys::SystemTrackingProperties {
                orientation_tracking: xr::sys::FALSE,
                position_tracking: xr::sys::FALSE,
            },
        };
        let result = unsafe {
            (self.instance.fp().get_system_properties)(
                self.instance.as_raw(),
                self.system,
                &mut sys_props,
            )
        };
        if result != xr::sys::Result::SUCCESS || ht_props.supports_hand_tracking == xr::sys::FALSE {
            return false;
        }

        // Load function pointers.
        let load_pfn = |name: &[u8]| -> Option<xr::sys::pfn::VoidFunction> {
            let mut pfn: Option<xr::sys::pfn::VoidFunction> = None;
            let result = unsafe {
                (self.instance.fp().get_instance_proc_addr)(
                    self.instance.as_raw(),
                    name.as_ptr() as *const _,
                    &mut pfn,
                )
            };
            if result == xr::sys::Result::SUCCESS {
                pfn
            } else {
                None
            }
        };

        let fp_create: xr::sys::pfn::CreateHandTrackerEXT =
            match load_pfn(b"xrCreateHandTrackerEXT\0") {
                Some(f) => unsafe { std::mem::transmute(f) },
                None => return false,
            };
        let fp_locate: xr::sys::pfn::LocateHandJointsEXT =
            match load_pfn(b"xrLocateHandJointsEXT\0") {
                Some(f) => unsafe { std::mem::transmute(f) },
                None => return false,
            };

        // Create one tracker per hand.
        let create_tracker = |hand: xr::sys::HandEXT| -> Option<xr::sys::HandTrackerEXT> {
            let info = xr::sys::HandTrackerCreateInfoEXT {
                ty: xr::sys::StructureType::HAND_TRACKER_CREATE_INFO_EXT,
                next: std::ptr::null(),
                hand,
                hand_joint_set: xr::sys::HandJointSetEXT::DEFAULT,
            };
            let mut handle = xr::sys::HandTrackerEXT::default();
            let result = unsafe { (fp_create)(session.session.as_raw(), &info, &mut handle) };
            if result == xr::sys::Result::SUCCESS && handle != xr::sys::HandTrackerEXT::default() {
                Some(handle)
            } else {
                None
            }
        };

        session.left_hand_tracker = create_tracker(xr::sys::HandEXT::LEFT);
        session.right_hand_tracker = create_tracker(xr::sys::HandEXT::RIGHT);
        session.pfn_locate_hand_joints = Some(fp_locate);

        session.left_hand_tracker.is_some() || session.right_hand_tracker.is_some()
    }
}

/// Frame-pacing counters for the real graphics session, refreshed each frame.
///
/// All timings are CPU-side wall-clock nanoseconds captured inside the `OpenXR`
/// frame loop. Only compiled with `--features metrics`; read via
/// [`XrGraphicsSession::pacing_metrics`].
#[cfg(feature = "metrics")]
#[derive(Debug, Clone, Copy, Default)]
pub struct XrPacingMetrics {
    /// Time blocked in `xrWaitFrame` (`waiter.wait()`) on the last frame. A
    /// *shrinking* wait as controllers are added is the signature of
    /// approaching the compositor's frame budget.
    pub wait_frame_ns: u64,
    /// Time blocked in `xrWaitSwapchainImage` across both eyes on the last
    /// frame (a GPU-bound stall).
    pub acquire_wait_ns: u64,
    /// Wall time between the last two `begin_frame` calls — the actual displayed
    /// cadence (period). Rising above the headset's native frame period is a
    /// dropped-frame signature.
    pub frame_interval_ns: u64,
    /// The runtime's predicted display time for the last frame, in nanoseconds.
    pub predicted_display_ns: i64,
    /// Number of frames submitted with **no** composition layer (`end_empty`) —
    /// hard reprojections and a flicker source.
    pub empty_frame_count: u64,
    /// Total frames begun since the session started.
    pub frame_count: u64,
    /// Whether a depth composition layer (`XR_KHR_composition_layer_depth`) was
    /// submitted on the last rendered frame. `true` once the runtime advertises
    /// the extension and `submit_frame` receives a depth range; `false` when the
    /// extension is unavailable or the depth range is `None`.
    pub depth_layer_submitted: bool,
}

/// A graphics-bound session + swapchains + frame loop, generic over the `OpenXR`
/// graphics API `G` (D3D12 on Windows PCVR, Vulkan on Quest). Only the raw
/// swapchain-image accessors differ per backend and live in `session.rs` /
/// `session_vk.rs`; everything here is shared.
/// The controller button / axis actions, grouped to keep the session struct
/// readable. Created in [`XrInstance::finish_session_setup`], bound to the Meta
/// Touch interaction profile, and read each frame by
/// [`XrGraphicsSession::read_controller_input`].
struct ControllerInputActions {
    left_thumbstick: xr::Action<xr::Vector2f>,
    right_thumbstick: xr::Action<xr::Vector2f>,
    left_trigger: xr::Action<f32>,
    right_trigger: xr::Action<f32>,
    left_squeeze: xr::Action<f32>,
    right_squeeze: xr::Action<f32>,
    /// `X` (left lower) / `A` (right lower).
    left_x: xr::Action<bool>,
    right_a: xr::Action<bool>,
    /// `Y` (left upper) / `B` (right upper).
    left_y: xr::Action<bool>,
    right_b: xr::Action<bool>,
    left_thumbstick_click: xr::Action<bool>,
    right_thumbstick_click: xr::Action<bool>,
    left_menu: xr::Action<bool>,
}

pub struct XrGraphicsSession<G: xr::Graphics> {
    pub(crate) instance: xr::Instance,
    pub(crate) session: xr::Session<G>,
    waiter: xr::FrameWaiter,
    stream: xr::FrameStream<G>,
    space: xr::Space,
    /// One swapchain per eye (`array_size = 1`).
    pub(crate) swapchains: [xr::Swapchain<G>; 2],
    /// One depth swapchain per eye, present only when the runtime advertised
    /// `XR_KHR_composition_layer_depth`. `None` disables the depth layer and
    /// falls back to color-only submission.
    pub(crate) depth_swapchains: Option<[xr::Swapchain<G>; 2]>,
    swap_width: u32,
    swap_height: u32,
    state: xr::SessionState,
    running: bool,
    /// Per-frame state, set by [`Self::begin_frame`] and consumed by
    /// [`Self::submit_frame`].
    frame_time: xr::Time,
    frame_views: Vec<xr::View>,
    should_render: bool,
    /// Action set for controller grip tracking.
    action_set: xr::ActionSet,
    /// Grip- and aim-pose actions per hand. Held only to keep the actions (and
    /// thus their anchored spaces below) alive for the session's lifetime — the
    /// per-frame pose reads go through the spaces, never these handles directly.
    #[allow(dead_code)]
    left_grip_action: xr::Action<xr::Posef>,
    #[allow(dead_code)]
    right_grip_action: xr::Action<xr::Posef>,
    /// Aim-pose actions for each hand (pointer-ray source).
    #[allow(dead_code)]
    left_aim_action: xr::Action<xr::Posef>,
    #[allow(dead_code)]
    right_aim_action: xr::Action<xr::Posef>,
    /// Action spaces anchored to each grip action.
    left_grip_space: xr::Space,
    right_grip_space: xr::Space,
    /// Action spaces anchored to each aim action.
    left_aim_space: xr::Space,
    right_aim_space: xr::Space,
    /// Controller button / axis actions (thumbsticks, triggers, grips, face
    /// buttons). Held both to keep them alive and to read their per-frame state.
    input_actions: ControllerInputActions,
    /// Hand trackers for `XR_EXT_hand_tracking` (both `None` when the
    /// feature is disabled or the device does not support the extension).
    #[cfg(feature = "hand-tracking")]
    left_hand_tracker: Option<xr::sys::HandTrackerEXT>,
    #[cfg(feature = "hand-tracking")]
    right_hand_tracker: Option<xr::sys::HandTrackerEXT>,
    /// Loaded `xrLocateHandJointsEXT` proc; `None` when hand tracking is
    /// unsupported.
    #[cfg(feature = "hand-tracking")]
    pfn_locate_hand_joints: Option<xr::sys::pfn::LocateHandJointsEXT>,
    /// Frame-pacing counters, refreshed each `begin_frame`/`submit_frame`.
    #[cfg(feature = "metrics")]
    pacing: XrPacingMetrics,
    /// Wall-clock timestamp of the previous `begin_frame`, to derive the
    /// displayed frame interval.
    #[cfg(feature = "metrics")]
    last_begin_instant: Option<std::time::Instant>,
}

impl<G: xr::Graphics> std::fmt::Debug for XrGraphicsSession<G> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XrGraphicsSession")
            .field("state", &format_args!("{:?}", self.state))
            .field("running", &self.running)
            .field("extent", &(self.swap_width, self.swap_height))
            .finish_non_exhaustive()
    }
}

impl<G: xr::Graphics> XrGraphicsSession<G> {
    /// Pump pending runtime events. Begins the session at `READY` and ends it
    /// at `STOPPING`. Returns `Ok(false)` once the runtime asks the app to
    /// exit (`EXITING` / `LOSS_PENDING`), `Ok(true)` otherwise.
    pub fn poll_events(&mut self) -> Result<bool, XrError> {
        let mut buffer = xr::EventDataBuffer::new();
        loop {
            let event = self
                .instance
                .poll_event(&mut buffer)
                .map_err(|_| XrError::SessionLost)?;
            let Some(event) = event else { break };
            // Diagnostic: log that *some* event arrived (events are rare, so this
            // is low-noise despite the per-frame poll). If events keep arriving
            // after the initial IDLE but none are SessionStateChanged(READY), the
            // runtime's event pipe is alive and it is deliberately withholding
            // READY (focus-gated); if nothing arrives, the runtime is silent.
            tracing::info!("xrPollEvent delivered an event");
            if let xr::Event::SessionStateChanged(e) = event {
                self.state = e.state();
                tracing::info!(state = ?self.state, "XR session state changed");
                match self.state {
                    xr::SessionState::READY => {
                        self.session
                            .begin(xr::ViewConfigurationType::PRIMARY_STEREO)
                            .map_err(|e| {
                                XrError::GraphicsBindingFailed(format!("begin session: {e:?}"))
                            })?;
                        self.running = true;
                    }
                    xr::SessionState::STOPPING => {
                        let _ = self.session.end();
                        self.running = false;
                    }
                    _ => {}
                }
            }
        }
        Ok(!matches!(
            self.state,
            xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING
        ))
    }

    /// Begin a frame: wait for the frame boundary, begin the frame stream,
    /// locate the stereo views, and acquire+wait both eye swapchain images.
    ///
    /// Returns `Some(XrFrame)` when the caller should render both eyes (a valid
    /// pose was located and images acquired). Returns `None` — after ending the
    /// frame with no layers — when the session is not running, the runtime says
    /// not to render this frame, or the pose is invalid; the caller must NOT
    /// call [`Self::submit_frame`] in that case.
    pub fn begin_frame(&mut self) -> Result<Option<XrFrame>, XrError> {
        if !self.running {
            return Ok(None);
        }
        #[cfg(feature = "metrics")]
        {
            let now = std::time::Instant::now();
            if let Some(prev) = self.last_begin_instant {
                self.pacing.frame_interval_ns = now.duration_since(prev).as_nanos() as u64;
            }
            self.last_begin_instant = Some(now);
            self.pacing.frame_count += 1;
        }
        #[cfg(feature = "metrics")]
        let t_wait = std::time::Instant::now();
        let fs = self
            .waiter
            .wait()
            .map_err(|e| XrError::GraphicsBindingFailed(format!("frame wait: {e:?}")))?;
        #[cfg(feature = "metrics")]
        {
            self.pacing.wait_frame_ns = t_wait.elapsed().as_nanos() as u64;
        }
        self.stream
            .begin()
            .map_err(|e| XrError::GraphicsBindingFailed(format!("frame begin: {e:?}")))?;
        self.frame_time = fs.predicted_display_time;
        self.should_render = fs.should_render;
        self.frame_views.clear();
        #[cfg(feature = "metrics")]
        {
            self.pacing.predicted_display_ns = self.frame_time.as_nanos();
        }

        if !fs.should_render {
            self.end_empty()?;
            return Ok(None);
        }

        let (flags, views) = self
            .session
            .locate_views(
                xr::ViewConfigurationType::PRIMARY_STEREO,
                self.frame_time,
                &self.space,
            )
            .map_err(|e| XrError::GraphicsBindingFailed(format!("locate_views: {e:?}")))?;
        if views.len() < 2
            || !flags.contains(xr::ViewStateFlags::POSITION_VALID)
            || !flags.contains(xr::ViewStateFlags::ORIENTATION_VALID)
        {
            self.end_empty()?;
            return Ok(None);
        }
        self.frame_views = views;

        let mut eyes = [XrEye::default(); 2];
        #[cfg(feature = "metrics")]
        let mut acquire_wait_ns = 0u64;
        for (i, eye) in eyes.iter_mut().enumerate() {
            let idx = self.swapchains[i]
                .acquire_image()
                .map_err(|e| XrError::GraphicsBindingFailed(format!("acquire_image: {e:?}")))?;
            #[cfg(feature = "metrics")]
            let t_img = std::time::Instant::now();
            self.swapchains[i]
                .wait_image(xr::Duration::INFINITE)
                .map_err(|e| XrError::GraphicsBindingFailed(format!("wait_image: {e:?}")))?;
            #[cfg(feature = "metrics")]
            {
                acquire_wait_ns += t_img.elapsed().as_nanos() as u64;
            }
            // Acquire+wait the matching depth image when a depth layer is active.
            let depth_image_index = if let Some(depth) = self.depth_swapchains.as_mut() {
                let didx = depth[i].acquire_image().map_err(|e| {
                    XrError::GraphicsBindingFailed(format!("depth acquire_image: {e:?}"))
                })?;
                depth[i].wait_image(xr::Duration::INFINITE).map_err(|e| {
                    XrError::GraphicsBindingFailed(format!("depth wait_image: {e:?}"))
                })?;
                didx
            } else {
                0
            };
            let v = &self.frame_views[i];
            *eye = XrEye {
                pose: LocatedPose {
                    position: [v.pose.position.x, v.pose.position.y, v.pose.position.z],
                    orientation: [
                        v.pose.orientation.x,
                        v.pose.orientation.y,
                        v.pose.orientation.z,
                        v.pose.orientation.w,
                    ],
                },
                fov: [
                    v.fov.angle_left,
                    v.fov.angle_right,
                    v.fov.angle_up,
                    v.fov.angle_down,
                ],
                image_index: idx,
                depth_image_index,
            };
        }
        #[cfg(feature = "metrics")]
        {
            self.pacing.acquire_wait_ns = acquire_wait_ns;
        }

        // Sync actions and locate controller grip spaces.
        let active_sets = [xr::ActiveActionSet::new(&self.action_set)];
        let _ = self.session.sync_actions(&active_sets); // non-fatal if no controller connected

        let left_controller = self.locate_grip(&self.left_grip_space);
        let right_controller = self.locate_grip(&self.right_grip_space);
        let left_aim = self.locate_grip(&self.left_aim_space);
        let right_aim = self.locate_grip(&self.right_aim_space);

        // Read controller button / axis state from the just-synced action set.
        let left_input = self.read_controller_input(true);
        let right_input = self.read_controller_input(false);

        Ok(Some(XrFrame {
            eyes,
            left_controller,
            right_controller,
            left_aim,
            right_aim,
            #[cfg(feature = "hand-tracking")]
            left_hand: self.locate_hand_joints(true),
            #[cfg(feature = "hand-tracking")]
            right_hand: self.locate_hand_joints(false),
            #[cfg(not(feature = "hand-tracking"))]
            left_hand: None,
            #[cfg(not(feature = "hand-tracking"))]
            right_hand: None,
            left_input,
            right_input,
        }))
    }

    /// Release both eye images and end the frame, submitting a stereo
    /// projection layer when `rendered` is `true`. Call exactly once after a
    /// [`Self::begin_frame`] that returned `Some`.
    ///
    /// When depth swapchains exist (`XR_KHR_composition_layer_depth` active) and
    /// `depth_range = Some((near, far))`, a [`sys::CompositionLayerDepthInfoKHR`]
    /// is chained onto each projection view so the compositor reprojects on real
    /// per-pixel depth. `near`/`far` are the projection's clip-plane distances
    /// (metres); the depth buffer is a right-handed `z ∈ [0, 1]` mapping, so
    /// `min_depth = 0.0` (near plane) and `max_depth = 1.0` (far plane).
    ///
    /// [`sys::CompositionLayerDepthInfoKHR`]: openxr::sys::CompositionLayerDepthInfoKHR
    pub fn submit_frame(
        &mut self,
        rendered: bool,
        depth_range: Option<(f32, f32)>,
    ) -> Result<(), XrError> {
        // Balance the two color acquires from begin_frame.
        let _ = self.swapchains[0].release_image();
        let _ = self.swapchains[1].release_image();
        // Balance the depth acquires too (begin_frame acquired one per eye).
        if let Some(depth) = self.depth_swapchains.as_mut() {
            let _ = depth[0].release_image();
            let _ = depth[1].release_image();
        }

        if rendered && self.frame_views.len() == 2 {
            let w = i32::try_from(self.swap_width).unwrap_or(i32::MAX);
            let h = i32::try_from(self.swap_height).unwrap_or(i32::MAX);
            let rect = xr::Rect2Di {
                offset: xr::Offset2Di { x: 0, y: 0 },
                extent: xr::Extent2Di {
                    width: w,
                    height: h,
                },
            };
            let mut proj_views = [
                xr::CompositionLayerProjectionView::new()
                    .pose(self.frame_views[0].pose)
                    .fov(self.frame_views[0].fov)
                    .sub_image(
                        xr::SwapchainSubImage::new()
                            .swapchain(&self.swapchains[0])
                            .image_array_index(0)
                            .image_rect(rect),
                    ),
                xr::CompositionLayerProjectionView::new()
                    .pose(self.frame_views[1].pose)
                    .fov(self.frame_views[1].fov)
                    .sub_image(
                        xr::SwapchainSubImage::new()
                            .swapchain(&self.swapchains[1])
                            .image_array_index(0)
                            .image_rect(rect),
                    ),
            ];

            // Build one depth-info per eye when a depth layer is active. Kept in
            // a local that outlives `stream.end()` below, since the raw `next`
            // pointers chained onto the projection views borrow it.
            let depth_infos: Option<[xr::sys::CompositionLayerDepthInfoKHR; 2]> =
                match (self.depth_swapchains.as_ref(), depth_range) {
                    (Some(depth), Some((near, far))) => {
                        let make = |sc: &xr::Swapchain<G>| xr::sys::CompositionLayerDepthInfoKHR {
                            ty: xr::sys::StructureType::COMPOSITION_LAYER_DEPTH_INFO_KHR,
                            next: std::ptr::null(),
                            sub_image: xr::sys::SwapchainSubImage {
                                swapchain: sc.as_raw(),
                                image_rect: rect,
                                image_array_index: 0,
                            },
                            min_depth: 0.0,
                            max_depth: 1.0,
                            near_z: near,
                            far_z: far,
                        };
                        Some([make(&depth[0]), make(&depth[1])])
                    }
                    _ => None,
                };

            if let Some(infos) = depth_infos.as_ref() {
                for (pv, info) in proj_views.iter_mut().zip(infos.iter()) {
                    // SAFETY: `CompositionLayerProjectionView` is
                    // `#[repr(transparent)]` over `sys::CompositionLayerProjectionView`,
                    // so the cast is layout-valid. `info` lives in `depth_infos`,
                    // which outlives the `stream.end()` call that reads the chain.
                    unsafe {
                        let raw = (pv as *mut xr::CompositionLayerProjectionView<'_, G>)
                            .cast::<xr::sys::CompositionLayerProjectionView>();
                        (*raw).next = std::ptr::from_ref(info).cast::<c_void>();
                    }
                }
            }

            let layer = xr::CompositionLayerProjection::new()
                .space(&self.space)
                .views(&proj_views);
            self.stream
                .end(self.frame_time, xr::EnvironmentBlendMode::OPAQUE, &[&layer])
                .map_err(|e| XrError::GraphicsBindingFailed(format!("frame end: {e:?}")))?;
            #[cfg(feature = "metrics")]
            {
                self.pacing.depth_layer_submitted = depth_infos.is_some();
            }
        } else {
            self.end_empty()?;
        }
        self.frame_views.clear();
        Ok(())
    }

    /// End the current frame with no composition layers.
    fn end_empty(&mut self) -> Result<(), XrError> {
        #[cfg(feature = "metrics")]
        {
            self.pacing.empty_frame_count += 1;
        }
        self.stream
            .end(self.frame_time, xr::EnvironmentBlendMode::OPAQUE, &[])
            .map_err(|e| XrError::GraphicsBindingFailed(format!("frame end (empty): {e:?}")))
    }

    /// Snapshot of the frame-pacing counters (see [`XrPacingMetrics`]).
    #[cfg(feature = "metrics")]
    #[must_use]
    pub fn pacing_metrics(&self) -> XrPacingMetrics {
        self.pacing
    }

    /// The per-eye swapchain resolution `(width, height)`.
    #[must_use]
    pub fn swapchain_extent(&self) -> (u32, u32) {
        (self.swap_width, self.swap_height)
    }

    /// Whether a depth composition layer is active for this session
    /// (`XR_KHR_composition_layer_depth` advertised and depth swapchains
    /// created). When `false`, the backend depth-image accessor returns `None`
    /// and [`Self::submit_frame`] ignores its `depth_range` argument.
    #[must_use]
    pub fn depth_swapchain_available(&self) -> bool {
        self.depth_swapchains.is_some()
    }

    /// The current runtime session state.
    #[must_use]
    pub fn state(&self) -> xr::SessionState {
        self.state
    }

    /// Whether the session has begun and not yet stopped.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Ask the runtime to end the session (drives toward `STOPPING`/`EXITING`).
    pub fn request_exit(&mut self) {
        let _ = self.session.request_exit();
    }

    /// Whether hand tracking was successfully initialised on this session.
    ///
    /// Returns `true` when at least one hand tracker was created via
    /// [`XrInstance::try_init_hand_tracking`].
    #[must_use]
    pub fn hand_tracking_supported(&self) -> bool {
        #[cfg(feature = "hand-tracking")]
        return self.left_hand_tracker.is_some() || self.right_hand_tracker.is_some();
        #[cfg(not(feature = "hand-tracking"))]
        return false;
    }

    /// Locate 26 hand joints for one hand at `self.frame_time`.
    ///
    /// Returns `None` when hand tracking is unsupported, the tracker handle
    /// is not present, the hand is untracked (`isActive == FALSE`), or any
    /// joint's validity flags are missing.
    #[cfg(feature = "hand-tracking")]
    fn locate_hand_joints(&self, left: bool) -> Option<Box<[LocatedHandJoint; 26]>> {
        use std::ffi::c_void;
        let tracker = if left {
            self.left_hand_tracker?
        } else {
            self.right_hand_tracker?
        };
        let pfn = self.pfn_locate_hand_joints?;

        let mut joint_locs = [xr::sys::HandJointLocationEXT {
            location_flags: xr::SpaceLocationFlags::EMPTY,
            pose: xr::sys::Posef {
                orientation: xr::sys::Quaternionf {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                    w: 1.0,
                },
                position: xr::sys::Vector3f {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
            },
            radius: 0.0,
        }; 26];

        let mut locations = xr::sys::HandJointLocationsEXT {
            ty: xr::sys::StructureType::HAND_JOINT_LOCATIONS_EXT,
            next: std::ptr::null_mut::<c_void>(),
            is_active: xr::sys::FALSE,
            joint_count: 26,
            joint_locations: joint_locs.as_mut_ptr(),
        };

        let locate_info = xr::sys::HandJointsLocateInfoEXT {
            ty: xr::sys::StructureType::HAND_JOINTS_LOCATE_INFO_EXT,
            next: std::ptr::null::<c_void>(),
            base_space: self.space.as_raw(),
            time: self.frame_time,
        };

        let result = unsafe { (pfn)(tracker, &locate_info, &mut locations) };
        if result != xr::sys::Result::SUCCESS || locations.is_active == xr::sys::FALSE {
            return None;
        }

        let required_flags = xr::SpaceLocationFlags::from_raw(
            xr::SpaceLocationFlags::POSITION_VALID.into_raw()
                | xr::SpaceLocationFlags::ORIENTATION_VALID.into_raw(),
        );

        let mut out = Box::new([LocatedHandJoint::default(); 26]);
        for (i, jl) in joint_locs.iter().enumerate() {
            if !jl.location_flags.contains(required_flags) {
                return None;
            }
            out[i] = LocatedHandJoint {
                position: [jl.pose.position.x, jl.pose.position.y, jl.pose.position.z],
                orientation: [
                    jl.pose.orientation.x,
                    jl.pose.orientation.y,
                    jl.pose.orientation.z,
                    jl.pose.orientation.w,
                ],
                radius: jl.radius.max(0.003),
            };
        }
        Some(out)
    }

    /// Locate a single action space in STAGE space at `self.frame_time`.
    ///
    /// Returns `None` when the space is not tracked or the location flags are
    /// missing `POSITION_VALID | ORIENTATION_VALID`.
    fn locate_grip(&self, grip_space: &xr::Space) -> Option<LocatedPose> {
        let loc = grip_space.locate(&self.space, self.frame_time).ok()?;
        if !loc.location_flags.contains(
            xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID,
        ) {
            return None;
        }
        let p = loc.pose;
        Some(LocatedPose {
            position: [p.position.x, p.position.y, p.position.z],
            orientation: [
                p.orientation.x,
                p.orientation.y,
                p.orientation.z,
                p.orientation.w,
            ],
        })
    }

    /// Read one controller's button / axis state from the current (already
    /// `sync_actions`-ed) action set. Any individual read failure (controller
    /// disconnected, action inactive) degrades that component to its rest value,
    /// never an error — a missing controller just reports a neutral frame.
    fn read_controller_input(&self, left: bool) -> ControllerInput {
        let a = &self.input_actions;
        let s = &self.session;
        let null = xr::Path::NULL;
        let f32_of = |act: &xr::Action<f32>| act.state(s, null).map_or(0.0, |st| st.current_state);
        let bool_of = |act: &xr::Action<bool>| act.state(s, null).is_ok_and(|st| st.current_state);
        let vec2_of = |act: &xr::Action<xr::Vector2f>| {
            act.state(s, null)
                .map_or([0.0, 0.0], |st| [st.current_state.x, st.current_state.y])
        };
        if left {
            ControllerInput {
                thumbstick: vec2_of(&a.left_thumbstick),
                trigger: f32_of(&a.left_trigger),
                grip: f32_of(&a.left_squeeze),
                thumbstick_click: bool_of(&a.left_thumbstick_click),
                primary: bool_of(&a.left_x),
                secondary: bool_of(&a.left_y),
                menu: bool_of(&a.left_menu),
            }
        } else {
            ControllerInput {
                thumbstick: vec2_of(&a.right_thumbstick),
                trigger: f32_of(&a.right_trigger),
                grip: f32_of(&a.right_squeeze),
                thumbstick_click: bool_of(&a.right_thumbstick_click),
                primary: bool_of(&a.right_a),
                secondary: bool_of(&a.right_b),
                // The Meta Touch right controller exposes no menu button.
                menu: false,
            }
        }
    }
}
