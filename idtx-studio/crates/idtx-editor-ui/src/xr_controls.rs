//! Runtime-preview control — the "Preview in Runtime" / "Stop Preview" menu
//! entry that launches the active stage in the out-of-process `idtx-runtime`
//! player (`OpenXR`).
//!
//! This module is deliberately **platform-agnostic**: it depends only on
//! `egui` and the UI-neutral [`EditorCommand`] vocabulary, never on any XR or
//! process crate. The shell owns the preview subprocess and feeds this function
//! a [`RuntimePreviewControl`] snapshot describing the gating state; the
//! function decides how the control renders and returns the command to
//! dispatch when the user activates it.
//!
//! Because the runtime process probes for an `OpenXR` device itself (and falls
//! back to a desktop preview when none is present), the editor no longer needs
//! device detection — the control gates only on the product and an open stage.

use egui::Key::C;
use idtx_editor_model::EditorCommand;

/// Snapshot of the gating inputs that decide how the runtime-preview control
/// renders. Cheap to build and `Copy`, so the shell can compute it once before
/// the egui pass and hand it in by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimePreviewControl {
    /// The running product exposes runtime preview at all
    /// ([`ProductProfile::xr_preview_enabled`][idtx_editor_model::ProductProfile::xr_preview_enabled]).
    /// When `false` the control draws nothing.
    pub enabled_for_product: bool,
    /// A stage is open in the active tab.
    pub stage_open: bool,
    /// A runtime-preview process is currently running for the active stage
    /// (show "Stop" instead of "Preview").
    pub preview_running: bool,
}

impl RuntimePreviewControl {
    /// `true` when a preview can be launched right now (stage open + not already
    /// previewing).
    #[must_use]
    pub const fn can_launch(self) -> bool {
        self.stage_open && !self.preview_running
    }
}

/// The launch commands the control should offer given `ctrl` — an XR preview
/// and a desktop (no-HMD) preview, or nothing when a launch is not
/// currently possible (product disabled, no stage, or a preview already
/// running). Pure so it is unit-testable without an egui context; the menu
/// renderer maps each returned command to a button.
#[must_use]
pub fn available_launch_commands(ctrl: RuntimePreviewControl) -> &'static [(EditorCommand, &'static str)] {
    if !ctrl.enabled_for_product || !ctrl.can_launch() {
        return &[];
    }
    &[
        (EditorCommand::LaunchRuntimePreview,"Preview in XR"),
        (EditorCommand::LaunchRuntimePreviewDesktop,"Preview in Desktop"),
    ]
}

/// Render the runtime-preview menu entry into `ui` and return the command to
/// dispatch, if the user activated it.
///
/// Renders nothing (and returns `None`) when the product does not support
/// runtime preview. When a preview is running it shows an enabled "Stop
/// Preview"; otherwise it shows a launch button per
/// [`available_launch_commands`] — an XR preview and a desktop (no-HMD)
/// preview — enabled only when a stage is open, with an on-hover explanation of
/// why they are disabled.
pub fn runtime_preview_menu_item(
    ui: &mut egui::Ui,
    ctrl: RuntimePreviewControl,
) -> Option<EditorCommand> {
    if !ctrl.enabled_for_product {
        return None;
    }

    if ctrl.preview_running {
        if ui.button("⏹  Stop Preview").clicked() {
            return Some(EditorCommand::StopRuntimePreview);
        }
        return None;
    }

    let can_launch = ctrl.can_launch();
    let mut chosen = None;
    // ensure to render all available commands, even no stage is open yet, to show them inactive with the
    // respective hover hint.
    for command in available_launch_commands(RuntimePreviewControl {
        stage_open: true,
        ..ctrl
    }) {
        let response = ui.add_enabled(can_launch, egui::Button::new(command.1));
        if response.clicked() {
            chosen = Some(command.0);
        }
        if !can_launch {
            response.on_disabled_hover_text("Open a USD stage first.");
        }
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_when_product_disabled() {
        let ctrl = RuntimePreviewControl {
            enabled_for_product: false,
            stage_open: true,
            preview_running: false,
        };
        // can_launch is independent of the product gate; the menu function
        // returns None early, which is exercised via egui in integration.
        assert!(ctrl.can_launch());
    }

    #[test]
    fn can_launch_requires_stage_and_idle() {
        let base = RuntimePreviewControl {
            enabled_for_product: true,
            stage_open: true,
            preview_running: false,
        };
        assert!(base.can_launch());
        assert!(
            !RuntimePreviewControl {
                stage_open: false,
                ..base
            }
            .can_launch()
        );
        assert!(
            !RuntimePreviewControl {
                preview_running: true,
                ..base
            }
            .can_launch()
        );
    }

    #[test]
    fn offers_xr_and_desktop_when_launchable() {
        let ctrl = RuntimePreviewControl {
            enabled_for_product: true,
            stage_open: true,
            preview_running: false,
        };
        assert_eq!(
            available_launch_commands(ctrl),
            &[
                (EditorCommand::LaunchRuntimePreview,"Preview in XR"),
                (EditorCommand::LaunchRuntimePreviewDesktop,"Preview in Desktop"),
            ],
            "an idle, stage-open control offers both an XR and a desktop preview"
        );
    }

    #[test]
    fn offers_nothing_when_not_launchable() {
        let base = RuntimePreviewControl {
            enabled_for_product: true,
            stage_open: true,
            preview_running: false,
        };
        // Product disabled, no stage, or already previewing → no launch buttons.
        assert!(
            available_launch_commands(RuntimePreviewControl {
                enabled_for_product: false,
                ..base
            })
            .is_empty()
        );
        assert!(
            available_launch_commands(RuntimePreviewControl {
                stage_open: false,
                ..base
            })
            .is_empty()
        );
        assert!(
            available_launch_commands(RuntimePreviewControl {
                preview_running: true,
                ..base
            })
            .is_empty()
        );
    }
}
