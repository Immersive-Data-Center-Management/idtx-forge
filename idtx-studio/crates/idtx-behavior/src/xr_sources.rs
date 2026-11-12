//! XR controller input source vocabulary (`Controller/<Type>/<Control>`).
//!
//! Extends the `Device/Control` input-source grammar (`Key/*`, `Mouse/Delta`,
//! `Gamepad/*`) with an XR-controller namespace so an `input_map.toml` can bind
//! logical actions to controller thumbsticks, triggers, grips and buttons:
//!
//! ```toml
//! [actions.move]            # 2D locomotion from the left thumbstick
//! kind = "axis2"
//! bindings = ["Controller/MetaQuest/LeftThumbstick"]
//! radial_deadzone = 0.15
//!
//! [actions.jump]            # a face button
//! kind = "button"
//! bindings = ["Controller/MetaQuest/A"]
//! ```
//!
//! # Controller type token
//!
//! The supported controllers are the **Meta Quest 2 / 3 / Pro**,
//! which share the Meta Touch button layout, so one canonical type token —
//! [`META_QUEST`] (`"MetaQuest"`) — covers all three. The sub-type aliases
//! `Quest`, `Quest2`, `Quest3` and `QuestPro` are accepted in author maps and
//! [`canonicalize_source`]d to `MetaQuest`, so a map written against a specific
//! device still matches the sources the runtime feeds.
//!
//! # Control identifiers
//!
//! Per hand (`Left*` / `Right*`), plus hand-specific face buttons:
//!
//! | Control                         | Fed as         | Usable as     |
//! |---------------------------------|----------------|---------------|
//! | `Left/RightThumbstick`          | 2D vector      | `axis2`       |
//! | `Left/RightThumbstickX`/`Y`     | analogue axis  | `axis`        |
//! | `Left/RightTrigger`             | analogue + button | `axis`/`button` |
//! | `Left/RightGrip`                | analogue + button | `axis`/`button` |
//! | `X`,`Y` (left) / `A`,`B` (right)| button         | `button`      |
//! | `Left/RightThumbstickPress`     | button         | `button`      |
//! | `Menu` (left)                   | button         | `button`      |
//!
//! A trigger / grip is fed both as its analogue value *and* as a digital press
//! once its value crosses [`BUTTON_THRESHOLD`], so the same source binds either
//! as a continuous `axis` or as a `button`.

use std::borrow::Cow;

use crate::input::FrameInput;

/// The device namespace for XR controller sources.
pub const CONTROLLER_NS: &str = "Controller";

/// The canonical controller type token for the Meta Quest 2 / 3 / Pro family.
pub const META_QUEST: &str = "MetaQuest";

/// Analogue triggers / grips report a digital press once their value reaches
/// this fraction of full travel, so a trigger can drive a `button` action.
pub const BUTTON_THRESHOLD: f32 = 0.5;

// Canonical full source strings (`Controller/MetaQuest/<control>`). These are
// exactly the strings an author writes in `input_map.toml` and the strings the
// runtime feeds onto the input router — the single source of truth for both.

/// Left thumbstick 2D vector (`axis2`).
pub const LEFT_THUMBSTICK: &str = "Controller/MetaQuest/LeftThumbstick";
/// Right thumbstick 2D vector (`axis2`).
pub const RIGHT_THUMBSTICK: &str = "Controller/MetaQuest/RightThumbstick";
/// Left thumbstick X component (scalar `axis`).
pub const LEFT_THUMBSTICK_X: &str = "Controller/MetaQuest/LeftThumbstickX";
/// Left thumbstick Y component (scalar `axis`).
pub const LEFT_THUMBSTICK_Y: &str = "Controller/MetaQuest/LeftThumbstickY";
/// Right thumbstick X component (scalar `axis`).
pub const RIGHT_THUMBSTICK_X: &str = "Controller/MetaQuest/RightThumbstickX";
/// Right thumbstick Y component (scalar `axis`).
pub const RIGHT_THUMBSTICK_Y: &str = "Controller/MetaQuest/RightThumbstickY";
/// Left trigger (analogue `axis` or `button`).
pub const LEFT_TRIGGER: &str = "Controller/MetaQuest/LeftTrigger";
/// Right trigger (analogue `axis` or `button`).
pub const RIGHT_TRIGGER: &str = "Controller/MetaQuest/RightTrigger";
/// Left grip / squeeze (analogue `axis` or `button`).
pub const LEFT_GRIP: &str = "Controller/MetaQuest/LeftGrip";
/// Right grip / squeeze (analogue `axis` or `button`).
pub const RIGHT_GRIP: &str = "Controller/MetaQuest/RightGrip";
/// `A` face button (right controller, lower).
pub const A: &str = "Controller/MetaQuest/A";
/// `B` face button (right controller, upper).
pub const B: &str = "Controller/MetaQuest/B";
/// `X` face button (left controller, lower).
pub const X: &str = "Controller/MetaQuest/X";
/// `Y` face button (left controller, upper).
pub const Y: &str = "Controller/MetaQuest/Y";
/// Left thumbstick press (click).
pub const LEFT_THUMBSTICK_PRESS: &str = "Controller/MetaQuest/LeftThumbstickPress";
/// Right thumbstick press (click).
pub const RIGHT_THUMBSTICK_PRESS: &str = "Controller/MetaQuest/RightThumbstickPress";
/// Menu button (left controller).
pub const MENU: &str = "Controller/MetaQuest/Menu";

/// Canonicalise an input-source string: rewrite the Quest sub-type aliases
/// (`Quest`, `Quest2`, `Quest3`, `QuestPro`) in a `Controller/<type>/<control>`
/// source to the canonical [`META_QUEST`] token, so a map authored against a
/// specific device matches the canonical sources the runtime feeds. Any source
/// that is not a Quest-family controller source is returned unchanged (borrowed).
#[must_use]
pub fn canonicalize_source(source: &str) -> Cow<'_, str> {
    let Some(rest) = source.strip_prefix("Controller/") else {
        return Cow::Borrowed(source);
    };
    let Some((ty, control)) = rest.split_once('/') else {
        return Cow::Borrowed(source);
    };
    if matches!(ty, "Quest" | "Quest2" | "Quest3" | "QuestPro") {
        Cow::Owned(format!("{CONTROLLER_NS}/{META_QUEST}/{control}"))
    } else {
        // Already canonical (`MetaQuest`) or an unknown type — leave as-is.
        Cow::Borrowed(source)
    }
}

/// One controller's per-frame input state (device-neutral plain data).
///
/// `primary` / `secondary` are the hand's lower / upper face buttons — `X`/`Y`
/// on the left controller, `A`/`B` on the right — so the same struct serves both
/// hands; [`apply_xr_controllers`] routes them to the correct source names.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
// Each bool is an independent physical button; they are not a state machine.
#[allow(clippy::struct_excessive_bools)]
pub struct XrHandInput {
    /// Thumbstick deflection, `[x, y]` each in `[-1, 1]` (x right+, y up+).
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
    /// Menu button (left controller only; ignored for the right hand).
    pub menu: bool,
}

/// Both controllers' per-frame input state.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct XrControllerSnapshot {
    /// Left controller.
    pub left: XrHandInput,
    /// Right controller.
    pub right: XrHandInput,
}

/// Feed a controller input snapshot onto `frame` as canonical
/// `Controller/MetaQuest/*` sources, so the input router resolves bound actions
/// from it exactly as it does keyboard / mouse / gamepad sources.
pub fn apply_xr_controllers(frame: &mut FrameInput, snap: &XrControllerSnapshot) {
    apply_hand(
        frame,
        &snap.left,
        HandSources {
            thumbstick: LEFT_THUMBSTICK,
            thumbstick_x: LEFT_THUMBSTICK_X,
            thumbstick_y: LEFT_THUMBSTICK_Y,
            trigger: LEFT_TRIGGER,
            grip: LEFT_GRIP,
            primary: X,
            secondary: Y,
            thumbstick_press: LEFT_THUMBSTICK_PRESS,
            menu: Some(MENU),
        },
    );
    apply_hand(
        frame,
        &snap.right,
        HandSources {
            thumbstick: RIGHT_THUMBSTICK,
            thumbstick_x: RIGHT_THUMBSTICK_X,
            thumbstick_y: RIGHT_THUMBSTICK_Y,
            trigger: RIGHT_TRIGGER,
            grip: RIGHT_GRIP,
            primary: A,
            secondary: B,
            thumbstick_press: RIGHT_THUMBSTICK_PRESS,
            // The Meta Touch right controller exposes no menu button.
            menu: None,
        },
    );
}

/// The canonical source names for one hand (fixed strings; see the `*` consts).
#[derive(Clone, Copy)]
struct HandSources {
    thumbstick: &'static str,
    thumbstick_x: &'static str,
    thumbstick_y: &'static str,
    trigger: &'static str,
    grip: &'static str,
    primary: &'static str,
    secondary: &'static str,
    thumbstick_press: &'static str,
    menu: Option<&'static str>,
}

fn apply_hand(frame: &mut FrameInput, input: &XrHandInput, src: HandSources) {
    // Thumbstick: a 2D vector source plus its scalar components, so authors can
    // bind either the whole stick (`axis2`) or a single axis (`axis`).
    frame.set_vector2(src.thumbstick, input.thumbstick);
    frame.set_analog(src.thumbstick_x, input.thumbstick[0]);
    frame.set_analog(src.thumbstick_y, input.thumbstick[1]);

    // Trigger / grip: analogue value, and a digital press past the threshold so
    // the same source also drives a `button` action.
    frame.set_analog(src.trigger, input.trigger);
    frame.set_analog(src.grip, input.grip);
    set_button(frame, src.trigger, input.trigger >= BUTTON_THRESHOLD);
    set_button(frame, src.grip, input.grip >= BUTTON_THRESHOLD);

    // Digital buttons.
    set_button(frame, src.primary, input.primary);
    set_button(frame, src.secondary, input.secondary);
    set_button(frame, src.thumbstick_press, input.thumbstick_click);
    if let Some(menu) = src.menu {
        set_button(frame, menu, input.menu);
    }
}

/// Press or release a digital source so the router derives Pressed/Held/Released
/// edges across frames.
fn set_button(frame: &mut FrameInput, source: &'static str, down: bool) {
    if down {
        frame.press(source);
    } else {
        frame.release(source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{InputMap, InputRouter};
    use idtx_behavior_abi::InputPhase;

    #[test]
    fn canonicalize_rewrites_quest_aliases() {
        for alias in ["Quest", "Quest2", "Quest3", "QuestPro"] {
            let src = format!("Controller/{alias}/A");
            assert_eq!(canonicalize_source(&src), "Controller/MetaQuest/A");
        }
    }

    #[test]
    fn canonicalize_leaves_canonical_and_other_sources_untouched() {
        // Already canonical → borrowed, unchanged.
        assert!(matches!(
            canonicalize_source("Controller/MetaQuest/LeftThumbstick"),
            Cow::Borrowed("Controller/MetaQuest/LeftThumbstick")
        ));
        // Non-controller sources pass straight through.
        assert!(matches!(
            canonicalize_source("Key/W"),
            Cow::Borrowed("Key/W")
        ));
        assert!(matches!(
            canonicalize_source("Mouse/Delta"),
            Cow::Borrowed("Mouse/Delta")
        ));
    }

    /// Build a router over a map, feed a snapshot, and return the resolved
    /// snapshot for assertions.
    fn routed(map_src: &str, snap: &XrControllerSnapshot) -> (InputRouter, crate::InputSnapshot) {
        let mut router = InputRouter::new(InputMap::parse(map_src).unwrap());
        let mut fi = FrameInput::new();
        apply_xr_controllers(&mut fi, snap);
        let out = router.snapshot(&fi.raw(), 0.016);
        (router, out)
    }

    #[test]
    fn move_axis2_reads_left_thumbstick_vector() {
        let src = r#"
[actions.move]
kind = "axis2"
bindings = ["Controller/MetaQuest/LeftThumbstick"]
"#;
        let mut snap = XrControllerSnapshot::default();
        snap.left.thumbstick = [0.5, -0.25];
        let (router, out) = routed(src, &snap);
        let v = out.axis2(router.resolve("move"));
        assert!((v[0] - 0.5).abs() < 1e-6 && (v[1] + 0.25).abs() < 1e-6);
    }

    #[test]
    fn move_forward_scalar_reads_thumbstick_y() {
        let src = r#"
[actions.move_forward]
kind = "axis"
bindings = ["Controller/MetaQuest/LeftThumbstickY"]
"#;
        let mut snap = XrControllerSnapshot::default();
        snap.left.thumbstick = [0.0, 0.8];
        let (router, out) = routed(src, &snap);
        assert!((out.axis(router.resolve("move_forward")) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn face_buttons_route_per_hand() {
        let src = r#"
[actions.a]
kind = "button"
bindings = ["Controller/MetaQuest/A"]
[actions.x]
kind = "button"
bindings = ["Controller/MetaQuest/X"]
"#;
        let mut snap = XrControllerSnapshot::default();
        snap.right.primary = true; // A (right lower)
        snap.left.primary = true; // X (left lower)
        let (router, out) = routed(src, &snap);
        assert_eq!(out.phase(router.resolve("a")), InputPhase::Pressed);
        assert_eq!(out.phase(router.resolve("x")), InputPhase::Pressed);
    }

    #[test]
    fn trigger_binds_as_both_axis_and_button() {
        let axis_src = r#"
[actions.throttle]
kind = "axis"
bindings = ["Controller/MetaQuest/RightTrigger"]
"#;
        let btn_src = r#"
[actions.fire]
kind = "button"
bindings = ["Controller/MetaQuest/RightTrigger"]
"#;
        let mut snap = XrControllerSnapshot::default();
        snap.right.trigger = 0.75; // past BUTTON_THRESHOLD

        let (ar, aout) = routed(axis_src, &snap);
        assert!((aout.axis(ar.resolve("throttle")) - 0.75).abs() < 1e-6);

        let (br, bout) = routed(btn_src, &snap);
        assert_eq!(bout.phase(br.resolve("fire")), InputPhase::Pressed);

        // Below the threshold the button does not fire but the axis still reads.
        let mut light = XrControllerSnapshot::default();
        light.right.trigger = 0.2;
        let (br2, bout2) = routed(btn_src, &light);
        assert_eq!(bout2.phase(br2.resolve("fire")), InputPhase::None);
    }

    #[test]
    fn alias_binding_matches_canonical_feed() {
        // Map authored against a specific sub-type still resolves the canonical
        // source the feed produces.
        let src = r#"
[actions.jump]
kind = "button"
bindings = ["Controller/Quest3/A"]
"#;
        let mut snap = XrControllerSnapshot::default();
        snap.right.primary = true;
        let (router, out) = routed(src, &snap);
        assert_eq!(out.phase(router.resolve("jump")), InputPhase::Pressed);
    }

    #[test]
    fn radial_deadzone_still_applies_to_stick_axis2() {
        let src = r#"
[actions.move]
kind = "axis2"
bindings = ["Controller/MetaQuest/LeftThumbstick"]
radial_deadzone = 0.2
"#;
        let mut snap = XrControllerSnapshot::default();
        snap.left.thumbstick = [0.1, 0.0]; // inside the deadzone → origin
        let (router, out) = routed(src, &snap);
        let v = out.axis2(router.resolve("move"));
        assert!(v[0].abs() < 1e-6 && v[1].abs() < 1e-6);
    }
}
