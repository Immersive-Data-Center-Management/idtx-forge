//! IDTX Conductor — the project-level authoring and orchestration app.
//!
//! A thin binary that launches the shared `idtx-studio-shell` editor with the
//! `ProductProfile::CONDUCTOR` profile. Given a project path on the command
//! line it opens that project; otherwise it opens the startup project dialog.

// On Windows, link the GUI binary against the "windows" subsystem for release
// builds so launching it (from Explorer or a shortcut) does NOT pop up a
// console window alongside the app. Debug builds keep the default "console"
// subsystem so `cargo run` still shows the `--help`/usage text, `eprintln!`
// errors and the `tracing` diagnostics below during development. No-op on
// non-Windows targets.
//
// Trade-off: in a *release* build launched from an existing terminal, the
// `--help`/usage output and `tracing` events have no console to attach to and
// are therefore not shown. The CLI surface is a development/automation aid; the
// shipped Conductor is a GUI app. If release-build console output from a parent
// terminal is ever needed, attach to it explicitly via
// `AttachConsole(ATTACH_PARENT_PROCESS)` before `init_tracing`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use idtx_editor_model::ProductProfile;
use idtx_project::IdtxProject;

/// Raw bytes of the crate's window/taskbar icon, embedded at compile
/// time. The same `.ico` is compiled into the Windows `.exe` resource
/// table by `build.rs` (Explorer file icon); this copy feeds winit's
/// runtime `Window::set_window_icon` so the *title-bar / taskbar* icon
/// also matches. See `idtx-studio-shell::ProductProfile::window_icon`.
const WINDOW_ICON: &[u8] = include_bytes!("../idtx_conductor_64.ico");

const USAGE: &str = "\
usage: idtx-conductor [PROJECT_FILE]
       idtx-conductor --project <PROJECT_FILE>

  PROJECT_FILE   path to an idtx project manifest (project.toml). When given,
                 the project is loaded and its default stage is opened. When
                 omitted, the Conductor opens the startup project dialog.";

/// Parsed command line for the Conductor binary.
struct Cli {
    /// Explicit project-manifest file to load on startup, if any.
    project_file: Option<PathBuf>,
    /// `-h`/`--help` was requested; print usage and exit.
    help: bool,
}

impl Cli {
    /// Parse the process arguments (`--project <file>` / `-p <file>` /
    /// `--project=<file>` / a bare positional `PROJECT_FILE` / `-h`/`--help`).
    fn from_env() -> Result<Self, String> {
        Self::parse(std::env::args_os().skip(1))
    }

    /// Pure parser over an argument iterator, factored out of [`Self::from_env`]
    /// so it can be unit-tested without touching the real process arguments.
    fn parse(args: impl Iterator<Item = OsString>) -> Result<Self, String> {
        let mut project_file: Option<PathBuf> = None;
        let mut help = false;
        let mut args = args;
        while let Some(arg) = args.next() {
            // Match on an owned lossy copy so the original `OsString` stays free
            // to move into a `PathBuf` in the positional arm (a borrow of `arg`
            // held by the match scrutinee would forbid that move).
            let flag = arg.to_string_lossy().into_owned();
            match flag.as_str() {
                "-h" | "--help" => help = true,
                "-p" | "--project" => {
                    let value = args
                        .next()
                        .ok_or_else(|| format!("{flag} requires a file path"))?;
                    project_file = Some(PathBuf::from(value));
                }
                // A `--project=<file>` form.
                s if s.starts_with("--project=") => {
                    project_file = Some(PathBuf::from(&s["--project=".len()..]));
                }
                s if s.starts_with('-') && s != "-" => {
                    return Err(format!("unknown option {s}"));
                }
                // A bare positional argument is the project file (kept as the
                // original `OsString` so non-UTF-8 paths survive intact).
                _ => {
                    if project_file.is_some() {
                        return Err("more than one project file given".to_string());
                    }
                    project_file = Some(PathBuf::from(arg));
                }
            }
        }
        Ok(Self { project_file, help })
    }
}

/// Install a `tracing` subscriber so the shell's cook / build / deploy
/// diagnostics (`info!`/`warn!`) actually reach the console. Without one every
/// `tracing` event is silently dropped — which is why the Vision Pro "Build and
/// deploy" path could stage-only or fail `devicectl` install with no visible
/// clue. Honours `RUST_LOG`; defaults to `info` so the deploy decision path
/// (creds resolved? device paired? install ok?) is visible out of the box.
fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn main() -> ExitCode {
    init_tracing();
    let cli = match Cli::from_env() {
        Ok(cli) => cli,
        Err(message) => {
            eprintln!("idtx-conductor: {message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    if cli.help {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    // With a project file, load it now (via the pure `idtx-project` model) and
    // hand the whole project to the shell, which applies its unit scale and
    // opens its default stage. Loading failures are fatal — the caller asked for
    // a specific project, so silently opening an empty window would hide the
    // error. With no project file, ask the shell to show the startup project
    // dialog (create/select).
    let startup = match &cli.project_file {
        Some(file) => match IdtxProject::load(file) {
            Ok(project) => idtx_studio_shell::Startup {
                project: Some(project),
                ..Default::default()
            },
            Err(err) => {
                eprintln!(
                    "idtx-conductor: failed to load project {}: {err}",
                    file.display()
                );
                return ExitCode::FAILURE;
            }
        },
        None => idtx_studio_shell::Startup {
            prompt_for_project: true,
            ..Default::default()
        },
    };

    if let Err(err) = idtx_studio_shell::run_startup(
        ProductProfile {
            window_icon: Some(WINDOW_ICON),
            ..ProductProfile::CONDUCTOR
        },
        startup,
    ) {
        eprintln!("idtx-conductor: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn parse(args: &[&str]) -> Result<Cli, String> {
        Cli::parse(args.iter().map(OsString::from))
    }

    #[test]
    fn no_args_means_no_project() {
        let cli = parse(&[]).unwrap();
        assert!(cli.project_file.is_none());
        assert!(!cli.help);
    }

    #[test]
    fn positional_project_file() {
        let cli = parse(&["proj/project.toml"]).unwrap();
        assert_eq!(cli.project_file, Some(PathBuf::from("proj/project.toml")));
    }

    #[test]
    fn project_flag_forms() {
        assert_eq!(
            parse(&["--project", "a/project.toml"])
                .unwrap()
                .project_file,
            Some(PathBuf::from("a/project.toml"))
        );
        assert_eq!(
            parse(&["-p", "b/project.toml"]).unwrap().project_file,
            Some(PathBuf::from("b/project.toml"))
        );
        assert_eq!(
            parse(&["--project=c/project.toml"]).unwrap().project_file,
            Some(PathBuf::from("c/project.toml"))
        );
    }

    #[test]
    fn help_flag_is_recorded() {
        assert!(parse(&["--help"]).unwrap().help);
        assert!(parse(&["-h"]).unwrap().help);
    }

    #[test]
    fn project_flag_without_value_errors() {
        assert!(parse(&["--project"]).is_err());
    }

    #[test]
    fn unknown_option_errors() {
        assert!(parse(&["--nope"]).is_err());
    }

    #[test]
    fn two_positionals_error() {
        assert!(parse(&["a.toml", "b.toml"]).is_err());
    }
}
