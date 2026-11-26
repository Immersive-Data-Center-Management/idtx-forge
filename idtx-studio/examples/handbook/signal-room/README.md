# Signal Room

A small USD-native experience for the [developer handbook](../../../docs/handbook/README.md): two rooms reference one station asset. A console pauses a rotating beacon and travels between rooms. The pause state survives travel.

The project uses only local files and the Studio built-in player. Copy this directory to a new location before customizing it. Run the helper from its original repository location; its optional `--project-root` argument selects your copied project. Conductor also compiles sources directly in the copied project.

## Open and explore

From the `idtx-studio` workspace:

```powershell
cargo run -p idtx-conductor -- examples/handbook/signal-room/project.toml
```

The Hub opens first. Use the scene tree to inspect `World/Beacon` and `World/Console`. The Console carries the `signal_console` behavior, and `World/Pointer` carries `signal_pointer`. Sources resolve through their defining asset layer; Conductor compiles them for playback, preview and cooking.

Run **Preview in Runtime** to test the current room with the built-in player. Look at the console and activate **Pause / Run** with the left mouse button. Look at the beacon to see its green outline. Desktop movement uses WASD and mouse look. The baseline XR rig uses its usual locomotion bindings; the example's pointer follows the active view, including the HMD view, and clicks use the right trigger, A button, or supported pinch gestures.

Use a **cooked project package** to test **Next room**. Current-stage preview does not provide the complete cooked level registry. A bare `--stage` launch also lacks the project transition table.

## Files

| File | Purpose |
|---|---|
| `project.toml` | Ordered Hub/Lab registry, start level, units and build settings |
| `input_map.toml` | Baseline player actions and `widgets.click` |
| `assets/station.usda` | Geometry, camera, light, collider, widget panel, behavior attachments |
| `levels/hub.usda` | Blue beacon; 1.5 radians/second; travel target `lab` |
| `levels/lab.usda` | Amber beacon; 0.75 radians/second; travel target `hub` |
| `behaviors/console.rs` | Typed configuration, rotation, widget callbacks and carried state |
| `behaviors/pointer.rs` | Camera raycast, widget routing and transient outline |
| `extensions/` | Optional prepared specimen, spawning behavior and opt-in Lab layer |
| `extensions/` | Optional prepared-prototype spawn exercise covered in the extension chapter |
| `tools/` | Source tests, project/input validation and optional OpenUSD validation |

## Validate

From the `idtx-studio` workspace:

```powershell
cargo test --manifest-path examples/handbook/signal-room/tools/Cargo.toml
cargo run --manifest-path examples/handbook/signal-room/tools/Cargo.toml -- check
cargo run --manifest-path examples/handbook/signal-room/tools/Cargo.toml -- compile
```

To validate or compile a working copy while keeping the helper's Cargo dependencies in their original layout:

```powershell
cargo run --manifest-path examples/handbook/signal-room/tools/Cargo.toml -- check --project-root target/signal-room-work
cargo run --manifest-path examples/handbook/signal-room/tools/Cargo.toml -- compile --project-root target/signal-room-work
```

The compiler command uses `idtx_behavior_build::build_behavior` and publishes check artifacts to `build/behaviors/`, including the optional spawn exercise's source. It does not stamp their paths into the source USD; Conductor stamps its own preview/cook layers. Native tests verify rotation across frame rates, bounded debugger pauses and typed-context errors. Native SDK host calls are inert, so those tests do not demonstrate live rendering, widget dispatch or travel.

With the pinned native OpenUSD install configured and its libraries on the loader path:

```powershell
cargo run --manifest-path examples/handbook/signal-room/tools/Cargo.toml --features usd -- usd
```

This opens both composed levels, checks the applied behavior schema and required prims, then decodes the actual composed configuration using the SDK. See the handbook's [testing chapter](../../../docs/handbook/src/testing.md) for runtime and device acceptance checks.

For a bounded headless render using compiled modules, prepare generated smoke wrappers and run a freshly built runtime:

```powershell
cargo run --manifest-path examples/handbook/signal-room/tools/Cargo.toml -- prepare-smoke
cargo run -p idtx-runtime --features metrics -- --stage examples/handbook/signal-room/build/smoke-hub.usda --mode desktop --headless --frames 120 --screenshot examples/handbook/signal-room/build/hub.png
```

This checks a standalone composed level, behavior attachment and rendering. The generated wrapper adds a local-role marker and stationary camera placement for a readable view; it does not add a player rig or the project transition table. Use Conductor preview for player interaction and a cooked package for travel.

## Expected result

1. Hub shows a blue rotating beacon and `RUNNING` on the panel.
2. Pause stops rotation and changes the readout to `PAUSED`.
3. In a cooked package, Next room opens Lab with an amber beacon; it stays paused.
4. Run resumes Lab's beacon at half Hub's speed.
5. Next room returns to Hub; the current pause/run state is retained.

The sample uses a hard cut. Add a registered transition stage through the handbook's composition workflow when you need a loading mask. There is no physics simulation scene in this sample: its USD collision schemas support scene queries and the XR locomotion queries, while the beacon animation is controlled by the behavior.

Continue with the [optional spawning exercise](../../../docs/handbook/src/tutorial/05-spawning.md) to add a prepared USD asset and its Ready callback.

