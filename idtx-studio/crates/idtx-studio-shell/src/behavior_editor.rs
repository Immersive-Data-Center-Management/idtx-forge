//! VS Code adapter for the editor-independent behavior development recipe.
//! Workspace preparation, profile setup and process launch run off the UI thread.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Mutex, mpsc::{Receiver, TryRecvError}};

use idtx_behavior_build::{DevelopmentContext, DevelopmentOptions, prepare_development};
use idtx_editor_ui::inspector::panels::behavior::{BehaviorEditorState, set_behavior_editor};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorAction { Open, Regenerate, Setup }

struct EditorJob {
    session_id: u64,
    prim_path: String,
    rx: Receiver<BehaviorEditorState>,
}

static EDITOR_JOBS: Mutex<Vec<EditorJob>> = Mutex::new(Vec::new());

pub(crate) fn start(
    ctx: &egui::Context,
    session_id: u64,
    prim_path: &str,
    project_root: PathBuf,
    source: PathBuf,
    sdk: PathBuf,
    action: EditorAction,
) {
    let mut jobs = EDITOR_JOBS.lock().expect("behavior editor jobs poisoned");
    if jobs.iter().any(|j| j.session_id == session_id && j.prim_path == prim_path) { return; }
    let (tx, rx) = std::sync::mpsc::channel();
    let repaint = ctx.clone();
    std::thread::spawn(move || {
        let progress = |message: &str| {
            let _ = tx.send(BehaviorEditorState::Preparing(message.to_owned()));
            repaint.request_repaint();
        };
        let result = (|| {
            progress("Preparing Cargo workspace and resolving the behavior SDK…");
            let workspace = prepare_workspace(&project_root, &source, &sdk, action == EditorAction::Regenerate)?;
            progress("Checking VS Code and rust-analyzer setup…");
            open_workspace(&workspace, workspace.context.source.entry(), action, &progress)
        })();
        let state = result.unwrap_or_else(BehaviorEditorState::Failed);
        let _ = tx.send(state);
        repaint.request_repaint();
    });
    set_behavior_editor(ctx, session_id, prim_path, BehaviorEditorState::Preparing("Preparing editor workspace…".into()));
    jobs.push(EditorJob { session_id, prim_path: prim_path.to_owned(), rx });
}

pub(crate) fn poll(ctx: &egui::Context) {
    EDITOR_JOBS.lock().expect("behavior editor jobs poisoned").retain(|job| {
        let mut finished = false;
        loop {
            match job.rx.try_recv() {
                Ok(state) => {
                    finished = !matches!(state, BehaviorEditorState::Preparing(_));
                    set_behavior_editor(ctx, job.session_id, &job.prim_path, state);
                    ctx.request_repaint();
                }
                Err(TryRecvError::Empty) => return !finished,
                Err(TryRecvError::Disconnected) => {
                    if !finished {
                        set_behavior_editor(ctx, job.session_id, &job.prim_path, BehaviorEditorState::Failed("Editor worker disconnected".into()));
                    }
                    return false;
                }
            }
        }
    });
}

struct EditorWorkspace {
    path: PathBuf,
    context: DevelopmentContext,
}

fn normalized(path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map(|p| idtx_project::strip_verbatim_prefix(&p)).map_err(|e| format!("{}: {e}", path.display()))
}

fn path_text(path: &Path) -> Result<String, String> {
    path.to_str().map(str::to_owned).ok_or_else(|| format!("Path is not UTF-8: {}", path.display()))
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if std::fs::read(path).is_ok_and(|old| old == bytes) { return Ok(()); }
    // Publish whole generated documents so an already-running editor never
    // observes a partially written workspace/registration list.
    let temporary = tempfile::NamedTempFile::new_in(path.parent().ok_or("Generated file has no parent")?)
        .map_err(|e| e.to_string())?;
    std::fs::write(temporary.path(), bytes).map_err(|e| e.to_string())?;
    temporary.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

fn prepare_workspace(project_root: &Path, entry: &Path, sdk: &Path, regenerate: bool) -> Result<EditorWorkspace, String> {
    let project = normalized(project_root)?;
    let entry = normalized(entry)?;
    let managed = project.join(".idtx/behavior-ide");
    std::fs::create_dir_all(&managed).map_err(|e| e.to_string())?;
    let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .open(managed.join("workspace.lock")).map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    write_if_changed(&managed.join(".gitignore"), b"# Generated, machine-local behavior editor data.\n*\n")?;

    // Registrations are entries the author actually opened, not every .rs file:
    // helper modules must never be treated as standalone WASM crates. Persist
    // project-relative paths so regeneration after moving a project repairs them.
    let registry = managed.join("sources.json");
    let mut sources: Vec<PathBuf> = std::fs::read(&registry).ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    sources = sources.into_iter().map(|p| project.join(p))
        .filter_map(|p| normalized(&p).ok()).filter(|p| p.is_file()).collect();
    sources.push(entry.clone());
    sources.sort();
    sources.dedup();
    let mut opts = DevelopmentOptions::new(&managed, sdk);
    opts.regenerate = regenerate;
    let contexts: Vec<_> = sources.iter().map(|p| prepare_development(p, &opts).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let workspace = workspace_document(&project, &managed, &contexts)?;
    let path = managed.join("behaviors.code-workspace");
    let text = format!("// Generated by Conductor. Use Regenerate Editor Workspace to repair.\n{}\n",
        serde_json::to_string_pretty(&workspace).map_err(|e| e.to_string())?);
    write_if_changed(&path, text.as_bytes())?;
    let registrations: Vec<_> = sources.iter().map(|p| p.strip_prefix(&project).unwrap_or(p)).collect();
    write_if_changed(&registry, &serde_json::to_vec_pretty(&registrations).map_err(|e| e.to_string())?)?;
    let context = contexts.into_iter().find(|c| c.source.entry() == entry).ok_or("Prepared entry disappeared")?;
    Ok(EditorWorkspace { path, context })
}

fn workspace_document(project: &Path, managed: &Path, contexts: &[DevelopmentContext]) -> Result<Value, String> {
    let context = contexts.first().ok_or("No behavior entries to prepare")?;
    let target = managed.join("target-ra");
    let mut roots: Vec<_> = contexts.iter().map(|c| c.source.root().to_path_buf()).collect();
    roots.sort();
    roots.dedup();
    let roots: Vec<_> = roots.iter().filter(|root| !roots.iter().any(|other| other != *root && root.starts_with(other))).collect();
    let folders: Vec<_> = roots.iter().map(|root| {
        let name = root.strip_prefix(project).unwrap_or(root);
        let name = if name.as_os_str().is_empty() { "Behavior Sources".into() } else { name.to_string_lossy() };
        json!({ "name": name, "path": root })
    }).collect();
    let mut readonly = serde_json::Map::new();
    for path in std::iter::once(managed).chain(contexts.iter().flat_map(|c| c.dependency_sources.iter().map(PathBuf::as_path))) {
        readonly.insert(format!("{}/**", path_text(path)?.replace('\\', "/")), json!(true));
    }
    let check_command = context.check_command(&target).map_err(|e| e.to_string())?;
    let tasks: Vec<_> = contexts.iter().map(|c| json!({
        "label": format!("Check {}", c.source.entry().strip_prefix(project).unwrap_or(c.source.entry()).display()),
        "type": "process", "command": c.cargo,
        "args": ["check", "--locked", "--release", "--lib", "--target", c.target,
            "--manifest-path", c.manifest_path, "--target-dir", target],
        "options": { "cwd": c.manifest_path.parent(), "env": c.environment },
        "problemMatcher": ["$rustc"], "group": "build",
    })).collect();
    Ok(json!({
        "folders": folders,
        "settings": {
            "rust-analyzer.linkedProjects": contexts.iter().map(|c| &c.manifest_path).collect::<Vec<_>>(),
            "rust-analyzer.vfs.extraIncludes": roots,
            "rust-analyzer.cargo.target": context.target,
            "rust-analyzer.cargo.allTargets": false,
            "rust-analyzer.check.allTargets": false,
            "rust-analyzer.cfg.setTest": false,
            "rust-analyzer.cargo.cfgs": ["!debug_assertions", "!miri"],
            "rust-analyzer.procMacro.enable": true,
            "rust-analyzer.cargo.buildScripts.enable": true,
            "rust-analyzer.cargo.extraArgs": ["--locked"],
            "rust-analyzer.checkOnSave": true,
            "rust-analyzer.check.extraArgs": ["--release", "--lib"],
            "rust-analyzer.cargo.targetDir": target,
            "rust-analyzer.cargo.buildScripts.overrideCommand": check_command,
            "rust-analyzer.cargo.buildScripts.invocationStrategy": "per_workspace",
            "rust-analyzer.check.overrideCommand": check_command,
            "rust-analyzer.check.invocationStrategy": "per_workspace",
            "rust-analyzer.server.extraEnv": context.environment,
            "rust-analyzer.cargo.extraEnv": context.environment,
            "rust-analyzer.cargo.sysroot": context.sysroot,
            "rust-analyzer.cargo.sysrootSrc": context.sysroot_src,
            "rust-analyzer.rustfmt.overrideCommand": [context.rustfmt, "--edition", "2021", "--emit", "stdout", "--config", "skip_children=true"],
            "rust-analyzer.imports.preferNoStd": true,
            "files.readonlyInclude": readonly,
            "files.exclude": { "**/.idtx": true, "**/.behavior-cache": true },
            "search.exclude": { "**/.idtx": true, "**/.behavior-cache": true },
            "[rust]": { "editor.defaultFormatter": "rust-lang.rust-analyzer" }
        },
        "extensions": { "recommendations": ["rust-lang.rust-analyzer"] },
        "tasks": { "version": "2.0.0", "tasks": tasks }
    }))
}

fn open_workspace(workspace: &EditorWorkspace, source: &Path, action: EditorAction, progress: &impl Fn(&str)) -> Result<BehaviorEditorState, String> {
    let detail = format!("{}\nSDK: {}\nWorkspace: {}", workspace.context.rustc_version,
        workspace.context.sdk_path.display(), workspace.path.display());
    if action != EditorAction::Setup {
        if let Ok(template) = std::env::var("IDTX_BEHAVIOR_EDITOR") {
            let argv = override_arguments(&template, &workspace.path, source)?;
            let mut child = Command::new(&argv[0]).args(&argv[1..])
                .envs(&workspace.context.environment).spawn().map_err(|e| format!("IDTX_BEHAVIOR_EDITOR: {e}"))?;
            // GUI executables can remain alive for the whole editing session.
            // Reap them without holding preparation in the Running state.
            std::thread::spawn(move || { let _ = child.wait(); });
            return Ok(BehaviorEditorState::Ready(detail));
        }
    }
    let program = find_vscode()?;
    let profile = std::env::var("IDTX_BEHAVIOR_VSCODE_PROFILE").unwrap_or_else(|_| "IDTX Behavior".into());
    let installed = launch_vscode(&program, &profile, workspace, source, action, progress)?;
    let profile_label = if profile.is_empty() { "default profile" } else { &profile };
    let detail = format!("{detail}\nVS Code profile: {profile_label}");
    if installed {
        Ok(BehaviorEditorState::Ready(detail))
    } else {
        Ok(BehaviorEditorState::SetupRequired(format!("rust-analyzer is not installed in {profile_label}. Use Set up rust-analyzer below to enable completion and diagnostics.\n{detail}")))
    }
}

fn launch_vscode(program: &Path, profile: &str, workspace: &EditorWorkspace, source: &Path, action: EditorAction, progress: &impl Fn(&str)) -> Result<bool, String> {
    // Opening a workspace creates a previously unknown named profile. The
    // extension-management CLI alone instead fails with "Profile ... not found".
    progress("Opening behavior workspace in VS Code…");
    let mut command = vscode_command(program, profile);
    command.envs(&workspace.context.environment).args(open_arguments(&workspace.path, source)?);
    checked_output(&mut command, "open VS Code behavior workspace")?;
    let mut list = vscode_command(program, profile);
    let available = checked_output(list.arg("--list-extensions"), "list VS Code extensions")?;
    let mut installed = String::from_utf8_lossy(&available.stdout).lines()
        .any(|line| line.trim().eq_ignore_ascii_case("rust-lang.rust-analyzer"));
    if action == EditorAction::Setup && !installed {
        progress("Installing rust-analyzer in the selected VS Code profile…");
        checked_output(vscode_command(program, profile).args(["--install-extension", "rust-lang.rust-analyzer"]), "install rust-analyzer")?;
        let available = checked_output(vscode_command(program, profile).arg("--list-extensions"), "verify rust-analyzer installation")?;
        installed = String::from_utf8_lossy(&available.stdout).lines()
            .any(|line| line.trim().eq_ignore_ascii_case("rust-lang.rust-analyzer"));
        if !installed { return Err("VS Code did not make rust-analyzer available in the selected profile. Retry Set up rust-analyzer.".into()); }
    }
    Ok(installed)
}

fn checked_output(command: &mut Command, operation: &str) -> Result<Output, String> {
    let output = command.output().map_err(|e| format!("Could not {operation}: {e}"))?;
    if !output.status.success() {
        return Err(format!("Could not {operation} ({}):\n{}{}", output.status,
            String::from_utf8_lossy(&output.stderr), String::from_utf8_lossy(&output.stdout)));
    }
    Ok(output)
}

fn vscode_command(program: &Path, profile: &str) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW for CLI setup/checks.
    }
    if !profile.is_empty() { command.args(["--profile", profile]); }
    command
}

fn open_arguments(workspace: &Path, source: &Path) -> Result<Vec<String>, String> {
    // A stable workspace lets VS Code reuse its matching window. --reuse-window
    // would target the last active window, including an unrelated project.
    Ok(vec![path_text(workspace)?, "--goto".into(), format!("{}:1:1", path_text(source)?)])
}

fn override_arguments(template: &str, workspace: &Path, file: &Path) -> Result<Vec<String>, String> {
    // JSON argv works on all platforms (including backslashes in Windows paths).
    // Keep legacy command templates, but parse their quoting before substitution
    // so spaces in the real file/workspace paths never split an argument.
    let mut argv: Vec<String> = if template.trim_start().starts_with('[') {
        serde_json::from_str(template).map_err(|e| format!("IDTX_BEHAVIOR_EDITOR must be a JSON argument array: {e}"))?
    } else {
        shlex::split(template).ok_or("Unclosed quote in IDTX_BEHAVIOR_EDITOR")?
    };
    if argv.is_empty() || argv[0].is_empty() { return Err("IDTX_BEHAVIOR_EDITOR has no executable".into()); }
    let file = path_text(file)?;
    let workspace = path_text(workspace)?;
    let has_file = argv.iter().any(|s| s.contains("{file}"));
    for argument in &mut argv {
        *argument = argument.replace("{workspace}", &workspace).replace("{file}", &file);
    }
    if !has_file { argv.push(file); }
    Ok(argv)
}

fn find_vscode() -> Result<PathBuf, String> {
    let candidates = vscode_candidates();
    for program in candidates {
        if vscode_command(&program, "").arg("--version").output().is_ok_and(|o| o.status.success()) { return Ok(program); }
    }
    Err("VS Code could not be launched. Install VS Code and its 'code' command, or set IDTX_BEHAVIOR_VSCODE to the executable path (IDTX_BEHAVIOR_EDITOR supports a custom command).".into())
}

fn vscode_candidates() -> Vec<PathBuf> {
    if let Some(program) = std::env::var_os("IDTX_BEHAVIOR_VSCODE") { return vec![program.into()]; }
    let mut paths = vec![PathBuf::from("code")];
    #[cfg(target_os = "macos")]
    {
        paths.push("/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code".into());
        if let Some(home) = dirs::home_dir() { paths.push(home.join("Applications/Visual Studio Code.app/Contents/Resources/app/bin/code")); }
    }
    #[cfg(windows)]
    {
        paths.push("code.cmd".into());
        for (variable, suffix) in [("LOCALAPPDATA", "Programs/Microsoft VS Code/bin/code.cmd"), ("ProgramFiles", "Microsoft VS Code/bin/code.cmd")] {
            if let Some(root) = std::env::var_os(variable) { paths.push(PathBuf::from(root).join(suffix)); }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        paths.push("/usr/bin/code".into());
        paths.push("/snap/bin/code".into());
    }
    paths
}

#[cfg(test)]
#[path = "behavior_editor_lsp_test.rs"]
mod lsp_test;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_arguments_preserve_spaces_quotes_and_workspace_routing() {
        let file = Path::new("/project with spaces/behaviors/é.rs");
        let workspace = Path::new("/project with spaces/.idtx/behaviors.code-workspace");
        assert_eq!(override_arguments(r#"["/editor path/code", "--profile", "My Profile", "{workspace}", "--goto", "{file}:4:2"]"#, workspace, file).unwrap(),
            ["/editor path/code", "--profile", "My Profile", workspace.to_str().unwrap(), "--goto", "/project with spaces/behaviors/é.rs:4:2"]);
        assert_eq!(override_arguments(r#""/editor path/code" "{workspace}" '{file}'"#, workspace, file).unwrap(),
            ["/editor path/code", workspace.to_str().unwrap(), file.to_str().unwrap()]);
        assert!(override_arguments("code 'unclosed", workspace, file).is_err());
        let args = open_arguments(workspace, file).unwrap();
        assert_eq!(args, [workspace.to_str().unwrap(), "--goto", "/project with spaces/behaviors/é.rs:1:1"]);
    }

    #[test]
    fn workspace_reuses_entries_repairs_paths_and_sets_up_an_empty_profile() {
        let temporary = tempfile::tempdir().unwrap();
        let project = temporary.path().join("project with spaces");
        let first = project.join("behaviors/one/behavior.rs");
        let second = project.join("behaviors/two/behavior.rs");
        for path in [&first, &second] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "// WIP sources can open before they compile.\nfn incomplete(").unwrap();
        }
        let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("idtx-behavior-sdk");
        let initial = prepare_workspace(&project, &first, &sdk, false).unwrap();
        prepare_workspace(&project, &second, &sdk, false).unwrap();
        let repeated = prepare_workspace(&project, &first, &sdk, false).unwrap();
        assert_eq!(repeated.path, initial.path);
        assert_eq!(repeated.context.manifest_path, initial.context.manifest_path);
        let document = |workspace: &Path| -> Value {
            let text = std::fs::read_to_string(workspace).unwrap();
            serde_json::from_str(text.split_once('\n').unwrap().1).unwrap()
        };
        let value = document(&repeated.path);
        let settings = &value["settings"];
        assert_eq!(settings["rust-analyzer.linkedProjects"].as_array().unwrap().len(), 2);
        assert_eq!(value["folders"].as_array().unwrap().len(), 1);
        assert_eq!(settings["rust-analyzer.cargo.target"], "wasm32-unknown-unknown");
        assert_eq!(settings["rust-analyzer.cargo.extraArgs"], json!(["--locked"]));
        assert_eq!(settings["rust-analyzer.check.overrideCommand"], settings["rust-analyzer.cargo.buildScripts.overrideCommand"]);
        let command = settings["rust-analyzer.check.overrideCommand"].as_array().unwrap();
        assert!(command.contains(&json!("--release")) && command.contains(&json!("--lib")));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let program = temporary.path().join("fake code");
            // Model the real CLI: extension commands fail for a missing profile;
            // opening a workspace creates it. An empty profile needs installation.
            std::fs::write(&program, r#"#!/bin/sh
root=$(dirname "$0")
for arg in "$@"; do
  case "$arg" in
    *.code-workspace) touch "$root/profile"; exit 0 ;;
    --list-extensions)
      test -f "$root/profile" || exit 2
      if test -f "$root/extension"; then echo rust-lang.rust-analyzer; fi
      exit 0 ;;
    --install-extension)
      test -f "$root/profile" || exit 2
      touch "$root/extension"; exit 0 ;;
  esac
done
exit 3
"#).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(!launch_vscode(&program, "Empty Profile", &repeated, &first, EditorAction::Open, &|_| {}).unwrap());
            assert!(launch_vscode(&program, "Empty Profile", &repeated, &first, EditorAction::Setup, &|_| {}).unwrap());
            assert!(launch_vscode(&program, "Empty Profile", &repeated, &first, EditorAction::Open, &|_| {}).unwrap());
        }

        let moved = temporary.path().join("moved project");
        std::fs::rename(&project, &moved).unwrap();
        let moved_entry = moved.join("behaviors/one/behavior.rs");
        let repaired = prepare_workspace(&moved, &moved_entry, &sdk, true).unwrap();
        let value = document(&repaired.path);
        assert_eq!(value["settings"]["rust-analyzer.linkedProjects"].as_array().unwrap().len(), 2);
        assert!(!serde_json::to_string(&value).unwrap().contains("project with spaces"));
        std::fs::remove_file(moved.join("behaviors/two/behavior.rs")).unwrap();
        let pruned = prepare_workspace(&moved, &moved_entry, &sdk, false).unwrap();
        assert_eq!(document(&pruned.path)["settings"]["rust-analyzer.linkedProjects"].as_array().unwrap().len(), 1);
    }
}
