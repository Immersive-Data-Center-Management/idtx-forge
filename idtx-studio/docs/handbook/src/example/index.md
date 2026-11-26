# Complete Signal Room sources

The companion project is distributed in `examples/signal-room/` inside the handbook package. Copy that whole folder to a writable location of your choice. Throughout the tutorial, `project/` means that location; it does not refer to an engine checkout.

You can also create the project from these Markdown pages alone. Save each complete listing under its stated path, preserving the directory structure and filenames. The base project uses the project, input-map, USD and behavior listings; the extension files are included but initially disabled. The README, agent instructions and ignore rules below complete the portable project.

If you have the downloaded handbook folder and Python 3.9 or newer, the optional materializer performs the same reconstruction. From the handbook folder, run:

```powershell
python tools/create_example.py "C:\Experiences\SignalRoom"
```

Choose your own destination. The materializer reads the exact file listings from these pages, uses only standard Python, and refuses to overwrite existing authored files. Python is not needed when copying the companion folder or saving the listings yourself. After reconstruction, launch packaged Conductor and choose **Project > Open Project…**, then select `project/project.toml`. Follow [setup](../setup.md) for the compiler prerequisites supplied separately from the book.

| Source page | Files |
|---|---|
| [Project and input](project-files.md) | Manifest and logical action bindings |
| [USD layers](usd-files.md) | Reusable station, Hub and Lab |
| [Behaviors](behavior-files.md) | Complete Console and Pointer Rust modules |
| [Optional spawn extension](spawn-files.md) | Prepared specimen, spawner layer and Rust module |

The files below are part of the project. Give your coding agent the location of the matching handbook so it can follow the public [agent guide](../reference/agent-guide.md) and [SDK reference](../reference/behavior-sdk.md).

## `README.md`

<!-- example-file: README.md -->

```markdown
# Signal Room

This is a portable, source-only experience project for packaged IDTX Conductor.
It needs no IDTX Studio checkout. Copy this whole directory before customizing.
The accompanying developer handbook contains every project file and the SDK
reference. Follow its setup chapter to install Rust, the WASM target, rust-src,
rustfmt and VS Code, and to confirm that Conductor includes its matching
behavior SDK and target runtime.

Launch Conductor. Use Project > Open Project… and select this project's
project.toml. Hub opens first. Select World/Console, use Cargo Check and
Verify capabilities in its Behavior inspector, then repeat for World/Pointer.
Use Preview > Preview in Desktop for the current room. Desktop movement uses
WASD and mouse look. Aim at the console and use the left mouse button for
Pause / Run. Looking at the beacon gives it a green outline.

Always start behavior edits from Conductor's Inspector > Behavior panel:
use Create Behavior… for a new behavior or Edit Code for an existing one.
Conductor generates the Cargo and VS Code workspace that connects your source
to its SDK and WASM target, enabling rust-analyzer completion and syntax/type
checks. Use Set up rust-analyzer if prompted. Save all files before Cargo Check,
Verify capabilities or Preview. Open each entry through the Inspector; opening
a loose Rust file does not establish the generated crate context.

If inherited controls are disabled, open assets/station.usda in Conductor and
select Console or Pointer there to edit/check the shared implementation. Return
to Hub or Lab for preview. Use Make Local Copy… for an intentional local variant,
and Regenerate Editor Workspace if generated paths/settings need repair.

Use Build > Build Desktop Experience and launch the generated bundle to test
Next room. Current-stage preview does not provide the complete level registry.
Pause Hub, enter Lab, confirm its amber beacon stays paused, run it, and return
to Hub. Its blue beacon should be running again. Closing the app resets state.

Project files:

- project.toml: Hub/Lab registry, entry point, units and delivery settings.
- input_map.toml: built-in player actions and widgets.click bindings.
- assets/station.usda: reusable world, widget panel and behavior attachments.
- levels/hub.usda and levels/lab.usda: room-specific color, title and context.
- behaviors/console.rs: rotation, typed context, buttons and carried state.
- behaviors/pointer.rs: active-camera queries, widget routing and outlining.
- extensions/: optional prepared-prototype spawning exercise, initially disabled.
- AGENTS.md: project contract and instructions for your coding agent.

The extension chapter explains how to enable the Lab sublayer and register
the specimen prototype. Do both before previewing that exercise.

Conductor owns compilation, generated module paths, preview and cooking.
The project deliberately contains no engine workspace or engine-dependent
command-line helper. Compile success does not prove input, live host behavior,
travel or target-device delivery; use the handbook's acceptance checks.
```

## `AGENTS.md`

<!-- example-file: AGENTS.md -->

```markdown
# Experience project contract

This directory is an experience project opened by the packaged IDTX Conductor
application. Do not assume access to IDTX Studio sources or engine build tools.
The developer should provide the location of the matching distributed handbook.
Resolve the following paths within its src directory before changing behavior:

- reference/agent-guide.md: coding-agent workflow and supported API contract.
- reference/behavior-sdk.md: public signatures, types, examples and limits.
- reference/behavior-host.md: capabilities, lifecycle and deferred operations.
- composition/project.md and composition/usd.md: manifest and layer contracts.
- testing.md and delivery/cooking.md: validation and packaged acceptance.

If the handbook is unavailable, preserve this example's proven APIs and report
which additional API contract you need. Do not invent SDK methods or dependencies.

Project layout and ownership:

- project.toml uses schema version 1. All manifest paths are project-relative.
- levels/*.usda are Y-up, metre-unit level layers with defaultPrim World and
  customLayerData.idtx.stageKind = level. They reference reusable assets.
- assets/station.usda owns the common world and authored behavior source paths.
- behaviors/*.rs are single-file Rust inputs to Conductor's embedded compiler.
- input_map.toml keeps the built-in player's actions plus widgets.click.
- Generated build/ and .idtx/ contents are disposable; do not hand-stamp modules.

Behavior rules:

- Always begin behavior editing from Conductor's Inspector > Behavior with
  Create Behavior… or Edit Code. Use its generated VS Code workspace for
  rust-analyzer completion and syntax/type checks; use Set up rust-analyzer
  when prompted. A loose Rust file lacks the generated Cargo/SDK context.
- Let Conductor manage .idtx/behavior-ide/; use Regenerate Editor Workspace
  to repair it. Edit the original source and save all entry/helper files before
  checks or preview. Report any Inspector/editor checks you could not perform.
- For inherited bindings, edit shared code through the owning asset's Inspector
  or use Make Local Copy… for a distinct implementation. Helper modules and
  include data must stay inside the behavior source collection.
- One behavior per source module; annotate an inherent impl with
  #[behavior(kind = "runtime")] or kind = "editor". The USD kind must match.
- Use idtx_behavior_sdk::prelude::* and documented SDK reexports. There is no
  project Cargo.toml for adding arbitrary guest dependencies.
- Authored IdtxBehaviorAPI attributes grant capabilities and declare inputs.
  Rust imports alone do not grant host access. Verify capabilities after edits.
- Decode immutable JSON context with ctx.context(&mut scratch). Use
  ContextString<N> when values must outlive the scratch buffer. For serde derives,
  use #[serde(crate = "idtx_behavior_sdk::serde")]. No generic param getter exists.
- Resolve object bindings through composed USD relationships from ctx.entity().
  Check resolved, total/written/truncated and every target status. Console uses
  idtx:behavior:beacon; USD remaps its target during level composition. Named
  relative children remain useful for fixed asset structure. JSON paths do not
  gain relationship remapping.
- Authored context is fixed during a hook. A changed valid context reattaches
  at reconciliation, resetting fields and rerunning attachment/start hooks.
- Clip transport now includes release and status/read_status. Declare named
  actor clips in USD; inspect settled status and animation completion/failure
  events. Queue acceptance alone does not prove transport application.
- Curve queries use metre arc distances, explicit batch index and output frame.
  PointerSource, input Action.from_source, CaptureToken/on_input_capture and
  widget handle_from preserve independent desktop/left/right ownership.
- Widgets.handle returns occlusion. handle_from separates occluded from handled;
  child clicks and on_panel_click background events have distinct kinds.
- Hydrated USD content supplies world matrices. get_xform() has no hydrated USD
  fallback; preserve local TRS by deriving it from parent/child world matrices.
- Stage edits are deferred. New spawns are not immediately queryable. Use the
  documented spawn_reference_async callback and wait for terminal Ready/Failed.
- Register prototype IDs in project.toml; use ASCII identifiers such as specimen.
  Behaviors cannot spawn arbitrary asset files or URLs.
- Handles and behavior state end with level teardown. Carry bounded data through
  ctx.level() state and resolve fresh entities on attachment. This is no save file.
- Register widgets with panel-relative child names. WASM-only callback registries
  require #[cfg(target_arch = "wasm32")] around registration in native-testable code.
- Native SDK host wrappers are inert stubs; native logic checks cannot establish
  rendering, host calls, input dispatch, spawn readiness or room travel.

Validation:

1. Save edited files and reopen the relevant layer in Conductor.
2. Run Behavior inspector Cargo Check, then Verify capabilities on each binding.
3. Use editor playback for a small behavior iteration, then Preview in Desktop
   for active-camera input, widgets and deferred edits. Use Preview in XR for XR.
4. Build the complete Desktop Experience and run Hub -> Lab -> Hub acceptance.
   Preview composes only the active stage; it does not prove multi-level travel.
5. Repeat acceptance on each actual target device with its supplied runtime.
6. Report what was tested and what requires developer interaction or hardware.
```

## `.gitignore`

<!-- example-file: .gitignore -->

```text
/.idtx/
/build/
```
