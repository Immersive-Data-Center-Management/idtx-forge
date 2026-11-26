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
