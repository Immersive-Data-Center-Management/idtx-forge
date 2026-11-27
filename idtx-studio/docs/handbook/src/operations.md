# 8. Diagnose, optimize and release

Diagnose the boundary where the result first differs from your expectation. Keep a small release record beside the package: authored project revision, Conductor/SDK/Runtime releases, target tools, cook result, content stamp and acceptance evidence.

## Diagnose a failed checkpoint

| Symptom | Inspect first | Corrective action |
|---|---|---|
| App exits before opening | Distribution contents and launch log | Restore the complete compatible bundle; report missing native libraries to the distributor |
| Empty or incomplete stage | Composition warnings, layer stack, defaultPrim and payloads | Correct relative asset paths and exported roots |
| Changes disappear after reopen/build | Saved layer and intended edit target | Save authored opinions to the correct source layer, reopen and rebuild |
| Behavior has no SDK completion or Rust diagnostics | Open workspace, VS Code profile and workspace trust | Start from Inspector > Behavior > **Create Behavior…** or **Edit Code**; use the generated workspace and **Set up rust-analyzer** in the selected profile |
| Editor preparation reports missing tools | Inspector diagnostic, Rust components and VS Code command | Install the WASM target and `rustup component add rust-src rustfmt`; make VS Code’s `code` command available, then retry **Edit Code** |
| Workspace uses old source/SDK paths after a move or update | Inspector workspace details and generated settings | Use **Regenerate Editor Workspace**; let Conductor rebuild the managed context |
| Edit Code is disabled on an inherited binding | Owning layer and Inspector inheritance message | Open the owning asset to edit shared code, or create a scene-tree override and use **Make Local Copy…** for a distinct implementation |
| Cargo Check fails | Full diagnostic, Rust PATH, SDK path and WASM target | Fix source/macro errors, install the guest target or restore the matching bundled SDK |
| Module checks but cannot attach | Binding kind, imports, grants and runtime compatibility | Match runtime/editor kind and declared capabilities; recompile with the supplied SDK |
| Query works but transform does not change | Deferred edit path and valid target | Use StageEdit for hydrated USD poses; inspect runtime diagnostics and next-frame result |
| Entity lookup differs in preview/cook | Composed owner namespace | Resolve named relatives of ctx.entity() and reacquire handles after travel |
| Inputs never fire | Binding input declarations and input_map.toml | Check exact action names/types and delivered keyboard/controller/gesture sources |
| Held button repeats an action | Pressed versus Held handling | Use a press edge for toggles and one-shot requests |
| Spawn ticket exists but children are absent | Terminal Ready/Failed callback | Wait for Ready before querying; inspect prototype registration and context |
| Travel fails in preview | Active-level preview versus full registry | Run the cooked experience and confirm exact level IDs in run.toml |
| Body refuses to fall | PhysicsScene, collider/body APIs and pose ownership | Test Runtime; remove conflicting animation/behavior ownership from a dynamic body |
| Requested convex hull does not collide | Prepared hull arrays | Bake hull data during authoring; runtime does not synthesize missing hulls |
| HTTP denied or failed | Capability, allowed host/address and origin policy | Use async APIs; configure the intended endpoint and supplied Runtime support |
| Quest output has no APK | Build note and Android SDK/JDK discovery | Supply the packaging tools, inspect their error and test the installed APK |
| Vision Pro deployment fails | Signing fields, paired device and Apple tool output | Correct certificate/profile/bundle ID and device pairing on the macOS build host |
| Copied package fails | run.toml, module/assets/plugin/library closure | Rebuild the missing closure and copy the complete finished package |

If the installed SDK lacks a documented API, or a supplied Runtime lacks a required compiled feature, use a compatible distribution. Adding a field to project.toml cannot upgrade the native application. The [compatibility map](appendices/implementation-map.md) identifies which component owns each requirement.

The default editor profile is **IDTX Behavior**. If VS Code discovery needs an explicit path, set `IDTX_BEHAVIOR_VSCODE` before launching Conductor. `IDTX_BEHAVIOR_VSCODE_PROFILE` selects another profile; an empty value uses VS Code’s default profile. Install rust-analyzer in the profile Conductor actually opens. Advanced custom editor commands use `IDTX_BEHAVIOR_EDITOR` with `{workspace}` and `{file}` placeholders; that override requires your own language-server setup. The managed VS Code flow in the [behavior walkthrough](tutorial/03-behaviors.md#start-every-edit-from-the-inspector) provides the documented completion and checking configuration.

## Capture a focused log

Launch the installed app from a terminal so you can retain its tracing output. For example:

```powershell
$env:RUST_LOG = "info"
& "C:/IDTX/Conductor/idtx-conductor.exe" --project "C:/IDTX/Projects/SignalRoom/project.toml"
```

Adjust paths for your installation. Use the smallest action sequence that reproduces the failure and keep the corresponding log excerpt. Record the selected level, behavior owner and target. Use static SDK log messages or bounded formatting; the single-file behavior environment does not provide an allocator-based format! facility.

## Measure representative content

Begin with Signal Room’s local geometry to establish a functional baseline. Add representative textures, lighting, animation and data requests, then measure cold load, warm interaction and a level swap on the target. Desktop viewport responsiveness alone is insufficient to choose headset budgets.

| Budget | What to measure and adjust |
|---|---|
| Frame delivery | Sustained device frame times and compositor behavior; simplify expensive views |
| Textures | Decoded sizes, residency and target decode profile; reduce oversized assets |
| Geometry | Visible draws, instances and skinned content; reuse assets and trim unseen detail |
| Lighting | Active lights, shadow cost and intended target views |
| Guest work | Fuel, queued bytes, callbacks and in-flight HTTP; avoid unbounded per-frame work |
| Transitions | Composition time, peak overlap memory and masking scene; keep transfer content small |
| Lifecycle | Suspend/resume, tracking recovery, thermals and source availability |

Use metrics provided by your supplied Runtime or platform tools. The [capture guide](appendices/screenshots.md) describes optional runtime PNG capture; availability depends on that distributed binary. Retain resolution, adapter/device, Runtime release and action sequence with reports. An unavailable timing sample is not evidence of zero cost.

The current cook stores some build settings without carrying them into generated runtime configuration; see [project settings](composition/project.md). Verify observed behavior and performance rather than assuming a configured frame-rate value is enforced.

## Release sequence

1. Save the authored stages and manifest. Check every binding and record the compatible SDK/Runtime releases.
2. Build each target, confirm the latest completion result and inspect the level/prototype inventory in the payload.
3. Run startup, both travel directions, button actions and the optional spawn exercise if enabled.
4. Copy the complete finished package into a clean location and repeat desktop acceptance.
5. Install and run each target artifact; exercise tracking, suspend/resume and a sustained session.
6. Store acceptance results, screenshots and performance reports with the content stamp and package identity. Mark unperformed checks explicitly.

Keep credentials out of source context and release records. Development-token embedding is a specific device-testing option, covered in [cooking](delivery/cooking.md); leave it off for distributed packages.

**Next:** [Extend the experience](extensions.md), or use the [SDK reference](reference/sdk-overview.md).
