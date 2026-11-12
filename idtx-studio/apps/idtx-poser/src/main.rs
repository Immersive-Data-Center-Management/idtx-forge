//! IDTX Poser — the transform and variant authoring app.
//!
//! A thin binary that launches the shared `idtx-studio-shell` editor with the
//! `ProductProfile::POSER` profile.

// On Windows, link the GUI binary against the "windows" subsystem for release
// builds so launching it (from Explorer or a shortcut) does NOT pop up a
// console window alongside the app. Debug builds keep the default "console"
// subsystem so `cargo run` still shows `eprintln!`/panic output during
// development. No-op on non-Windows targets.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use idtx_editor_model::ProductProfile;

/// Raw bytes of the crate's window/taskbar icon, embedded at compile
/// time. The same `.ico` is compiled into the Windows `.exe` resource
/// table by `build.rs` (Explorer file icon); this copy feeds winit's
/// runtime `Window::set_window_icon` so the *title-bar / taskbar* icon
/// also matches. See `idtx-studio-shell::ProductProfile::window_icon`.
const WINDOW_ICON: &[u8] = include_bytes!("../idtx_poser_64.ico");

fn main() {
    if let Err(err) = idtx_studio_shell::run(
        ProductProfile {
            window_icon: Some(WINDOW_ICON),
            ..ProductProfile::POSER
        },
        None,
    ) {
        eprintln!("idtx-poser: {err}");
        std::process::exit(1);
    }
}
