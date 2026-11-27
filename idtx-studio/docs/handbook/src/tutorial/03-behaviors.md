# Implement the behaviors

Signal Room has two [complete single-file Rust sources](../example/behavior-files.md) in `project/behaviors/`. `console.rs` owns the beacon's state and responds to panel clicks. `pointer.rs` raycasts from the active camera and routes the result to widgets or highlighting.

Conductor compiles each source against `idtx-behavior-sdk` for `wasm32-unknown-unknown`. A source file is the authoring input; a resolved `idtx:behavior:module` is what allows the runtime to attach executable behavior. The compiler and cook stamp module paths into generated layers.

## Start every edit from the Inspector

**Always begin in Conductor’s Inspector > Behavior panel.** Select the owning prim, then choose **Create Behavior…** for a new behavior or **Edit Code** to edit an existing behavior. These actions prepare the workspace that embeds the source in its generated Cargo crate and open it in VS Code with the matching SDK and WASM configuration. A loose `.rs` file, or the experience folder opened without this workspace, lacks that generated crate context.

1. Open your experience in Conductor and select the prim whose behavior you want to author. For Signal Room, use the existing Console or Pointer attachment. If its controls are inherited and disabled, follow [shared behaviors](#share-or-customize-an-implementation) below.
2. Choose **Create Behavior…** or **Edit Code**. Creation writes a template to `behaviors/<name>/behavior.rs` beside the edited stage, authors the source attachment and starts a Cargo check. Editing resolves the existing attachment’s source. Both prepare the editor workspace and open the authored file.
3. Wait for the Inspector’s preparation status. If it reports a missing extension, choose **Set up rust-analyzer**. Conductor installs the extension in its selected VS Code profile, **IDTX Behavior** by default. Follow [setup](../setup.md) for `rust-src`, `rustfmt` and the WASM target. Trust your project’s workspace in VS Code to enable Cargo and procedural macros.
4. Work in the opened workspace. rust-analyzer supplies SDK-aware code completion, go-to-definition, syntax/type diagnostics and Cargo checks on save. Replace the new template’s `todo!()` bodies before running it; a template can compile while still trapping when a hook executes.
5. Save all source and helper files, return to the Inspector, then run **Cargo Check** and **Verify capabilities** before playback or Runtime preview. These actions use saved files. Editor diagnostics do not verify capability grants or live scene effects.

Conductor keeps the generated editor files in `<project>/.idtx/behavior-ide/`, including `behaviors.code-workspace`. The generated Cargo wrappers point to your original author files, so you edit the files used by the experience. The workspace configures SDK dependencies, procedural macros, the installed Rust toolchain and the `wasm32-unknown-unknown` target. Completion can use your current editor buffer; Cargo diagnostics on save, Inspector checks and builds use the saved source.

Open each behavior entry through its Inspector action so Conductor registers it in the shared project workspace. Helper `.rs` files are loaded through their Rust modules and are not registered as independent behaviors. The workspace includes a **Check <source path>** task for each registered entry.

Treat `.idtx/behavior-ide/` as generated, machine-local data. Keep your source files in version control and let Conductor manage the Cargo manifests, wrappers and editor settings. Use **Regenerate Editor Workspace** after moving the project, changing the installed SDK/toolchain, or when workspace paths or generated settings need repair. It refreshes the generated context and reopens the workspace.

## Organize source and helper modules

Signal Room’s existing `behaviors/console.rs` and `behaviors/pointer.rs` remain supported. Newly created behaviors have a named directory and a `behavior.rs` entry, leaving room for ordinary Rust helper modules and source-relative include data:

```text
<stage-directory>/
  behaviors/
    beacon/
      behavior.rs     # Attached entry; can declare mod motion;
      motion.rs       # Helper module
      data/
        settings.json # Optional include_str! input
```

Keep modules and include data inside the source collection: the nearest enclosing `behaviors/` directory, or the entry’s parent directory when it is outside a `behaviors/` tree. Conductor preserves their relative paths when compiling or copying the collection. Use regular source/data files; generated folders, dot-prefixed files/directories and `.wasm` outputs are excluded, and source symlinks are rejected. You do not need to create a project `Cargo.toml` or edit a generated manifest to split a behavior into modules.

The [shared Rust module exercise](07-shared-modules.md) provides complete sources for two separate behavior entries that import the same helper. It walks through opening both entries from the Inspector, navigating to the shared file with rust-analyzer, and checking both consumers after changing it.

## Share or customize an implementation

**Attach Behavior…** reuses an entry already inside the project, so several attachments can intentionally share its implementation. Attaching an external entry imports its source collection, preserving helper modules and include data. Each live attachment still has its own behavior state and authored configuration.

For Signal Room, both rooms inherit their Console and Pointer bindings from `assets/station.usda`. To change the common implementation, open that asset in Conductor, select `/World/Console` or `/World/Pointer`, and use **Edit Code** in its Behavior inspector. Changes to the shared source affect every attachment using it. Return to the level for room-specific preview.

For a distinct implementation on one attachment, use **Make Local Copy…** in the Inspector. It copies the source collection, gives the attachment a fresh identity, preserves its composed behavior configuration and opens the editor workspace. If inherited controls are disabled, create the scene-tree override indicated by the Inspector to expose the local-copy action. **Overwrite Behavior…** creates a fresh template while retaining configuration; use it when you intend to replace the implementation. Context and object-target overrides can customize an inherited binding without copying its code.

## Attach source and grant capabilities

The reusable station authors the Console attachment:

```usda
def IdtxWidgetPanel "Console" (
    prepend apiSchemas = ["IdtxBehaviorAPI"]
)
{
    token idtx:behavior:kind = "runtime"
    string idtx:behavior:name = "signal_console"
    string idtx:behavior:version = "0.1.0"
    string idtx:behavior:uuid = "signal-room-console-v1"
    asset idtx:behavior:source = @../behaviors/console.rs@
    uniform token[] idtx:behavior:capabilities = [
        "scene_query", "widget", "stage_edit", "level_control"
    ]
    custom string idtx:behavior:context = '{"speed_radians":1.5,"target_level":"lab"}'
    rel idtx:behavior:beacon = </World/Beacon>
}
```

This snippet omits the panel's visual attributes and children; the complete [station layer](../example/usd-files.md) includes them. There is no behavior manifest sidecar. The applied schema and attributes contain the attachment contract.

| Grant | Why Console needs it |
|---|---|
| `scene_query` | Resolve the composed beacon relationship and read its world transform |
| `widget` | Register and receive button callbacks |
| `stage_edit` | Queue session-layer transforms and readout updates |
| `level_control` | Carry pause state and request travel |

The source's `#[behavior(kind = "runtime")]` and the USD `kind` agree. A capability token permits host imports in that bucket. A Rust import or macro annotation alone does not grant access.

## Decode typed configuration once

Room-specific values are authored as a JSON string on `idtx:behavior:context`. Use an owned contract when values must survive the hook's scratch buffer:

```rust
use idtx_behavior_sdk::prelude::*;

#[derive(Deserialize)]
#[serde(crate = "idtx_behavior_sdk::serde")]
pub struct ConsoleConfig {
    pub speed_radians: f32,
    pub target_level: ContextString<24>,
}
```

The serde crate annotation is necessary for a single-file behavior that receives serde through the SDK reexport. `ContextString<24>` stores bounded UTF-8 bytes inside the contract instead of borrowing the scratch buffer. The generated behavior crate provides the SDK and arrayvec; it does not assume arbitrary Cargo dependencies.

Inside `on_start`:

```rust
let mut scratch = [0u8; 256];
let config: ConsoleConfig = ctx.context(&mut scratch)?;
if !config.speed_radians.is_finite() || config.speed_radians < 0.0
    || config.target_level.is_empty()
{
    return Err(ErrorCode::InvalidArgument.into());
}
self.speed = config.speed_radians;
self.target = config.target_level;
```

The host pins context before attachment. It stays fixed during every hook. A valid authored change reattaches the binding at the next reconciliation boundary, resets its Rust fields and reruns attachment/start hooks. Invalid context prevents attachment and is reported. Save the intended layer, then verify the new configuration and initialization. The SDK still has no generic getter for legacy parameter attributes.

## Resolve reusable scene content

USD remaps the authored `idtx:behavior:beacon` relationship when the level body is re-rooted by assembly composition. The Console queries that ordered binding rather than interpreting a path string in JSON:

```rust
let mut targets = [scene::RelationshipTarget::default(); 1];
let binding = ctx.scene().relationship_targets(
    ctx.entity(), "idtx:behavior:beacon", &mut targets,
)?;
if !binding.resolved || binding.total != 1 || binding.truncated() {
    return Err(ErrorCode::InvalidArgument.into());
}
self.beacon = targets[0].entity();
let beacon = self.beacon.ok_or(ErrorCode::NotFound)?;
let parent_world = beacon.parent()?.world_matrix()?;
self.base = Transform::from_matrix(parent_world.inverse() * beacon.world_matrix()?);
```

The initial world transform comes from hydrated USD content. Deriving the local transform from parent and child world matrices avoids relying on a guest-written local ECS transform. `Transform::from_matrix` decomposes positive TRS transforms as used by this sample; avoid introducing shear into this simple animation.

The contract requires exactly one live target. Missing, empty, invalid, unloaded, unresolved and truncated relationships are different outcomes; the reference explains each. Relative named-child queries remain useful for fixed internal asset structure. A relationship can bind the same behavior to a different authored object without changing Rust source. This example resolves once at startup. To follow live relationship edits, read the updated snapshot/revision and refresh the stored target; an authored relationship change does not itself update a Rust field.

## Animate through the session layer

The Console struct stores `angle`, `speed`, `running`, the base pose and the beacon handle. `on_update` advances only while running:

```rust
if self.running {
    self.angle = advance_angle(self.angle, self.speed, dt);
    let mut pose = self.base;
    pose.rotation = self.base.rotation * Quat::from_rotation_y(self.angle);
    if let Some(beacon) = self.beacon {
        ctx.stage_edit().set_transform(beacon, &pose)?;
    }
}
```

`advance_angle` uses seconds, wraps by a full turn and clamps a long frame to 0.1 seconds. A debugger pause therefore produces a bounded jump. The stage edit is queued during the hook, applied afterwards and re-projected into the scene. Source USD files are not saved every frame.

The complete source restores the base pose in `on_stop` and resets the angle. This makes Stop/Play iteration predictable. For larger animation workloads, evaluate the dedicated animation playback APIs described in the [SDK reference](../reference/behavior-sdk.md), including their current hydration and routing constraints.

## Register panel callbacks

Register panel-relative child names in `on_start`:

```rust
#[cfg(target_arch = "wasm32")]
{
    ctx.widget().on_click("Toggle", Self::toggle_clicked)?;
    ctx.widget().on_click("Travel", Self::travel_clicked)?;
}
```

The target guard permits the same source to compile for native logic tests: the macro generates its widget registry only for WASM. The actual WASM build checks these callback calls.

The callback is a non-capturing function that receives mutable behavior state, context and the click handle:

```rust
fn toggle_clicked(&mut self, ctx: &Ctx, _click: &WidgetClick) -> Result<()> {
    self.running = !self.running;
    ctx.level().set_state("beacon.running", &[u8::from(self.running)])?;
    self.refresh_readout(ctx)
}
```

`refresh_readout` queues `set_widget_value("State", "RUNNING" or "PAUSED")` for the field under this panel. The guest does not require a general-purpose event-dispatch match statement; the macro routes the widget event to the registered callback.

## Compile and checkpoint

The complete project already supplies `console.rs` and `pointer.rs` under `project/behaviors/`, with attachments in the station layer. Begin edits through the Inspector’s **Edit Code** action as described above, then save all files in the generated workspace. Conductor supplies the Cargo context and SDK; you do not maintain a project `Cargo.toml`.

Select `/World/Console` and use the Behavior inspector in this order:

1. **Cargo Check**: checks the source for the WASM target and reports compile errors.
2. **Verify capabilities**: produces actual WASM and checks required, missing and unused host capability grants.
3. Editor Play/Stop transport: observes the rotating beacon and readout during local iteration.

Repeat both checks for `/World/Pointer`. If a shared station binding is inherited and the check controls are disabled, run the checks from its owning station asset, then return to the level for playback and preview. Apply permanent configuration changes to the intended authored layer and save; context changes reattach at reconciliation. Verify initialization after the resulting reset.

Conductor compiles and stamps generated modules for runtime preview and cooking. Do not write generated `idtx:behavior:module` paths back into the reusable source layer. A successful check or capability verification does not establish that host calls apply, a pointer can hit the panel, or a callback arrives. Continue with [interaction and travel](04-interactions.md).

The complete Console file contains three optional pure logic tests for frame-rate independence, long pauses and context decoding. See [testing](../testing.md) for the standalone SDK test route. Native wrappers are inert; run-time effects still need preview and cooked acceptance.

The [SDK reference](../reference/behavior-sdk.md) explains the exact public methods used here; the [host contract](../reference/behavior-host.md) explains grants, lifecycle and deferred edits.

Next: [Connect interaction and travel](04-interactions.md).
