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

use clap::Parser;
use idtx_editor_model::ProductProfile;
use idtx_studio_shell::cli::StageArgs;

#[derive(Debug, Parser)]
#[command(version, about = "Open a USD stage in IDTX Poser")]
struct Cli {
    #[command(flatten)]
    stage: StageArgs,
}

/// Raw bytes of the crate's window/taskbar icon, embedded at compile
/// time. The same `.ico` is compiled into the Windows `.exe` resource
/// table by `build.rs` (Explorer file icon); this copy feeds winit's
/// runtime `Window::set_window_icon` so the *title-bar / taskbar* icon
/// also matches. See `idtx-studio-shell::ProductProfile::window_icon`.
const WINDOW_ICON: &[u8] = include_bytes!("../idtx_poser_64.ico");

fn main() {
    // Release builds use the GUI subsystem. Attach to an existing terminal so
    // clap help and startup errors are visible, without creating a new console
    // when launched from Explorer.
    #[cfg(windows)]
    unsafe {
        // SAFETY: AttachConsole takes only a process identifier, no pointers.
        let _ = windows::Win32::System::Console::AttachConsole(
            windows::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
    if let Err(err) = run(Cli::parse()) {
        eprintln!("idtx-poser: {err}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let initial_stage = cli.stage.into_initial_stage(cfg!(feature = "remote"))?;
    idtx_studio_shell::run(
        ProductProfile {
            window_icon: Some(WINDOW_ICON),
            ..ProductProfile::POSER
        },
        initial_stage,
    )
    .map_err(|error| error.to_string())
}
