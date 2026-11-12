//! `SteamVR` action-manifest packaging profile (scaffold + doc).
//!
//! On Windows/Meta the runtime attaches `OpenXR` actions programmatically and lets
//! the Oculus runtime supply default bindings. **`SteamVR`** (and Monado on Linux)
//! instead want a JSON *action manifest* plus per-controller default-binding files
//! declared up front; the loader reads them from disk beside the app. This module
//! emits that manifest so a `linux-pcvr` / `SteamVR` bundle carries the input
//! contract the runtime already implements ([`CookTarget::pcvr_linux`]).
//!
//! ## Status
//!
//! Emitting the manifest JSON is pure and testable, so it lands here. What remains
//! is **host-bound** and deferred:
//! * wiring the packager to write `actions.json` + binding files into the bundle
//!   and pointing `xrCreateActionSet`/`SteamVR` at it (needs a `SteamVR` runtime to
//!   verify against — not present here);
//! * the Linux `OpenXR` **loader path**: `SteamVR` ships `libopenxr_loader.so` and
//!   registers `/usr/share/openxr/1/active_runtime.json`; Monado is the
//!   open-source alternative. A Linux bundle documents `XR_RUNTIME_JSON` as the
//!   override knob so a packaged experience can target either.
//!
//! The action set here mirrors the four actions the runtime attaches today
//! (`left_grip`, `right_grip`, `left_aim`, `right_aim`); adding an action is a
//! one-line change in [`default_action_set`] once the runtime grows one.
//!
//! [`CookTarget::pcvr_linux`]: crate::CookTarget::pcvr_linux

use std::fmt::Write;

/// One input action in the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    /// Full action path (`/actions/main/in/left_grip`).
    pub path: String,
    /// `SteamVR` action type (`pose`, `boolean`, `vector1`, …).
    pub kind: String,
}

/// A named action set with its actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionSet {
    /// Action-set path (`/actions/main`).
    pub path: String,
    /// The actions in this set.
    pub actions: Vec<Action>,
}

/// The default action set matching the runtime's attached `OpenXR` actions: grip +
/// aim poses for each hand. Kept in lockstep with `idtx-openxr-sys` session setup.
#[must_use]
pub fn default_action_set() -> ActionSet {
    let pose = |name: &str| Action {
        path: format!("/actions/main/in/{name}"),
        kind: "pose".to_string(),
    };
    ActionSet {
        path: "/actions/main".to_string(),
        actions: vec![
            pose("left_grip"),
            pose("right_grip"),
            pose("left_aim"),
            pose("right_aim"),
        ],
    }
}

/// Render a `SteamVR` `actions.json` manifest for `set`.
///
/// Emits the minimal, valid shape `SteamVR` expects: an `actions` array (each with
/// `name` + `type`) and an `action_sets` array. Default per-controller binding
/// files are referenced by convention (`bindings_<profile>.json`); generating
/// those bindings is the deferred host-side step.
#[must_use]
pub fn render_action_manifest(set: &ActionSet) -> String {
    let mut actions = String::new();
    for (i, a) in set.actions.iter().enumerate() {
        if i > 0 {
            actions.push_str(",\n");
        }
        let _ = write!(actions, 
            "    {{ \"name\": \"{}\", \"type\": \"{}\" }}",
            a.path, a.kind
        );
    }
    format!(
        "{{\n  \"actions\": [\n{actions}\n  ],\n  \"action_sets\": [\n    \
         {{ \"name\": \"{set}\", \"usage\": \"leftright\" }}\n  ],\n  \
         \"default_bindings\": [\n    {{ \"controller_type\": \"knuckles\", \
         \"binding_url\": \"bindings_knuckles.json\" }},\n    \
         {{ \"controller_type\": \"oculus_touch\", \"binding_url\": \
         \"bindings_oculus_touch.json\" }}\n  ]\n}}\n",
        set = set.path,
    )
}

/// The filename the manifest is written as inside a `SteamVR` bundle.
pub const ACTION_MANIFEST_FILE: &str = "actions.json";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_set_has_four_pose_actions() {
        let set = default_action_set();
        assert_eq!(set.path, "/actions/main");
        assert_eq!(set.actions.len(), 4);
        assert!(set.actions.iter().all(|a| a.kind == "pose"));
        assert!(set.actions.iter().any(|a| a.path.ends_with("left_aim")));
    }

    #[test]
    fn manifest_is_wellformed_and_lists_actions() {
        let json = render_action_manifest(&default_action_set());
        // Every action path appears, plus the set and default bindings.
        assert!(json.contains("/actions/main/in/left_grip"));
        assert!(json.contains("/actions/main/in/right_aim"));
        assert!(json.contains("\"action_sets\""));
        assert!(json.contains("knuckles"));
        assert!(json.contains("oculus_touch"));
        // Balanced braces — a cheap well-formedness check for the hand-rolled JSON.
        assert_eq!(
            json.chars().filter(|&c| c == '{').count(),
            json.chars().filter(|&c| c == '}').count()
        );
    }
}
