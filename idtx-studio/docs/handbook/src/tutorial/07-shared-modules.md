# Optional exercise: reuse a shared Rust module

Add two rotating objects to your writable Signal Room project. **SharedYaw** turns a blue bar around Y; **SharedRoll** turns an amber bar around Z. They are separate behavior entries that both import one `behaviors/shared/motion.rs` helper. Changing that helper changes both implementations when they are rebuilt; each attachment keeps its own angle and base pose.

Copy the [shared-modules companion extension](../../examples/shared-modules/README.md), or save the four complete listings below. Use the [Inspector authoring workflow](03-behaviors.md#start-every-edit-from-the-inspector) and the Rust/VS Code prerequisites in [setup](../setup.md).

## 1. Add the source collection and scene

Copy the companion's `behaviors/` and `extensions/` files into the corresponding directories of your **working Signal Room project**, preserving their relative paths. Keep the existing `project.toml` and `input_map.toml`. The resulting additions are:

```text
project/
  behaviors/
    shared/
      motion.rs             # One helper imported by both entries
    shared-yaw/
      behavior.rs           # Attached to /World/SharedYaw
    shared-roll/
      behavior.rs           # Attached to /World/SharedRoll
  extensions/
    shared-modules.usda      # Geometry and the two source attachments
  .idtx/behavior-ide/        # Generated later by the Inspector
```

Add the extension to the metadata of `levels/hub.usda`:

```usda
# Add inside the existing parenthesized layer metadata:
subLayers = [@../extensions/shared-modules.usda@]
```

If `subLayers` already exists, append the new entry to its array. Preserve Hub's station reference, metadata and room overrides. Save and reopen Hub in Conductor. The two bars appear beside the original console/beacon area.

## 2. Register both entries through the Inspector

**Always begin behavior editing in Conductor's Inspector > Behavior panel.** This example already has source attachments, so use **Edit Code** for each one:

1. Open `extensions/shared-modules.usda` in Conductor while working in the Signal Room project. Its own bindings are editable there; in Hub they are inherited from the sublayer.
2. Select `/World/SharedYaw` and choose **Edit Code**. Wait for the managed VS Code workspace to open. Use **Set up rust-analyzer** if prompted and trust your project's workspace.
3. Select `/World/SharedRoll` and choose **Edit Code** as well. This registers the second entry in the same project workspace, `.idtx/behavior-ide/behaviors.code-workspace`.
4. In either entry, use completion after `motion::`, then **Go to Definition** on `advance_angle`. Both entries lead to the same authored `behaviors/shared/motion.rs`. Open the helper through that module navigation or the workspace's file tree.

Conductor generates one Cargo context per behavior entry and links both into the editor workspace. The helper is a Rust module within each context: it has no `#[behavior]` implementation, USD attachment, UUID or separate Cargo package. Do not attach `motion.rs` as another behavior or add it as a generated Cargo project. Only the two `behavior.rs` files are behavior entries.

When building a similar example from scratch, select each object's prim and choose **Create Behavior…** first. Add the common helper within their enclosing `behaviors/` collection, then import it from the generated entry files. Adjust relative paths to match the locations Conductor created. Do not create or modify a generated `Cargo.toml` to share a helper.

## 3. Understand the shared import

Both entries declare the same module at the top level of their source file:

```rust
#[path = "../shared/motion.rs"]
mod motion;
```

The path is relative to each entry's directory: `shared-yaw/` and `shared-roll/` are siblings of `shared/`. Both paths therefore resolve to **one physical file** within the same enclosing `behaviors/` source collection. Use `motion::advance_angle(...)` from either behavior. Conductor's generated wrapper loads the entry as a normal Rust module, and the editor and production compiler preserve this relationship.

This shares source code across two compiled WASM modules. It does not share mutable runtime state: `angle` and `base` live in each behavior instance. The two entries use different axes and speeds, and the USD bindings have different identities. Reusing a single behavior attachment's source on several prims is another supported pattern, described in [shared implementations](03-behaviors.md#share-or-customize-an-implementation).

Keep the whole collection together when moving or distributing it. **Make Local Copy…** duplicates the source collection for a separate implementation; its copied helper can then diverge. To keep this exercise's two entries sharing future helper edits, continue editing their original common `motion.rs`.

## 4. Change the helper and check both consumers

Save all files, then run **Cargo Check** and **Verify capabilities** on both bindings in the extension's Inspector. Both require `scene_query` to capture the authored base pose and `stage_edit` to apply rotation. The shared helper itself performs only arithmetic.

Return to Hub and choose **Preview > Preview in Desktop**. Look toward the bars: SharedYaw turns around Y at 1 radian/second, while SharedRoll turns around Z at 0.5 radians/second. Stop preview before the following edit:

1. Return to the extension's Inspector and use **Edit Code** to open a registered entry in the managed workspace.
2. Navigate to `motion.rs` and change `SPEED_SCALE` from `1.0` to `2.0`. Save all files.
3. Check **both** consumers. Use each binding's **Cargo Check** and **Verify capabilities**. The generated VS Code **Check behaviors/shared-yaw/behavior.rs** and **Check behaviors/shared-roll/behavior.rs** tasks also provide explicit per-entry compilation checks; path separators in their labels depend on the platform. rust-analyzer diagnostics are useful while editing, but do not establish that every consuming binding has passed capability verification.
4. Start a fresh Desktop preview from Hub. Both bars should turn twice as fast, around their original axes. SharedYaw still turns twice as fast as SharedRoll. The original Signal Room beacon is unaffected because its behavior does not import this helper.
5. Restore `SPEED_SCALE` to `1.0`, save and check both consumers again.

The helper clamps a long frame to 0.1 seconds, so the stated speeds apply during normal frame delivery. Both behaviors reset their angle on start and restore their captured base pose on stop. Use a fresh preview after rebuilding; this exercise does not depend on an already-running Runtime hot-reloading edited source.

| Check | Expected result |
|---|---|
| Go to Definition from both entries | Opens the same `behaviors/shared/motion.rs` |
| Complete `motion::` in either entry | Offers `advance_angle` and `SPEED_SCALE` |
| Check both entries and verify their grants | Both compile and link with `scene_query` and `stage_edit` |
| Change the shared scale to 2.0 and rebuild | Both bars rotate twice as fast in a fresh preview |
| Stop and restart | Original placement is preserved and angles restart from zero |

## Complete authored files

Save each listing at its project-relative path, or copy the companion files. The existing Signal Room player, lights and floor supply the rest of the scene. Open code for editing through the Inspector after copying.

## behaviors/shared/motion.rs

<!-- shared-modules-file: behaviors/shared/motion.rs -->

```rust
//! Reused by both behavior entries; this file is not a behavior attachment.

pub const SPEED_SCALE: f32 = 1.0;

/// Advance in radians, bounding long frames and wrapping each full turn.
pub fn advance_angle(angle: f32, radians_per_second: f32, dt: f32) -> f32 {
    let step = dt.max(0.0).min(0.1);
    (angle + radians_per_second * SPEED_SCALE * step) % core::f32::consts::TAU
}
```

## behaviors/shared-yaw/behavior.rs

<!-- shared-modules-file: behaviors/shared-yaw/behavior.rs -->

```rust
use idtx_behavior_sdk::prelude::*;

#[path = "../shared/motion.rs"]
mod motion;

#[derive(Default)]
pub struct SharedYaw {
    base: Transform,
    angle: f32,
}

#[behavior(kind = "runtime")]
impl SharedYaw {
    fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
        let owner = ctx.entity();
        let parent_world = owner.parent()?.world_matrix()?;
        self.base = Transform::from_matrix(parent_world.inverse() * owner.world_matrix()?);
        self.angle = 0.0;
        Ok(())
    }

    fn on_update(&mut self, ctx: &Ctx, dt: f32) -> Result<()> {
        self.angle = motion::advance_angle(self.angle, 1.0, dt);
        let mut pose = self.base;
        pose.rotation = self.base.rotation * Quat::from_rotation_y(self.angle);
        ctx.stage_edit().set_self_transform(&pose)
    }

    fn on_stop(&mut self, ctx: &Ctx) -> Result<()> {
        self.angle = 0.0;
        ctx.stage_edit().set_self_transform(&self.base)
    }
}
```

## behaviors/shared-roll/behavior.rs

<!-- shared-modules-file: behaviors/shared-roll/behavior.rs -->

```rust
use idtx_behavior_sdk::prelude::*;

#[path = "../shared/motion.rs"]
mod motion;

#[derive(Default)]
pub struct SharedRoll {
    base: Transform,
    angle: f32,
}

#[behavior(kind = "runtime")]
impl SharedRoll {
    fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
        let owner = ctx.entity();
        let parent_world = owner.parent()?.world_matrix()?;
        self.base = Transform::from_matrix(parent_world.inverse() * owner.world_matrix()?);
        self.angle = 0.0;
        Ok(())
    }

    fn on_update(&mut self, ctx: &Ctx, dt: f32) -> Result<()> {
        self.angle = motion::advance_angle(self.angle, 0.5, dt);
        let mut pose = self.base;
        pose.rotation = self.base.rotation * Quat::from_rotation_z(self.angle);
        ctx.stage_edit().set_self_transform(&pose)
    }

    fn on_stop(&mut self, ctx: &Ctx) -> Result<()> {
        self.angle = 0.0;
        ctx.stage_edit().set_self_transform(&self.base)
    }
}
```

## extensions/shared-modules.usda

<!-- shared-modules-file: extensions/shared-modules.usda -->

```usda
#usda 1.0
(
    defaultPrim = "World"
    metersPerUnit = 1
    upAxis = "Y"
)

def Xform "World"
{
    def Xform "SharedYaw" (prepend apiSchemas = ["IdtxBehaviorAPI"])
    {
        double3 xformOp:translate = (-1.8, 1.0, -2.5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        token idtx:behavior:kind = "runtime"
        token idtx:behavior:scope = "entity"
        bool idtx:behavior:enabled = true
        string idtx:behavior:name = "shared_yaw"
        string idtx:behavior:version = "0.1.0"
        string idtx:behavior:uuid = "handbook-shared-yaw-v1"
        asset idtx:behavior:source = @../behaviors/shared-yaw/behavior.rs@
        uniform token[] idtx:behavior:capabilities = ["scene_query", "stage_edit"]

        def Cube "Body"
        {
            double size = 1
            double3 xformOp:scale = (0.7, 0.15, 0.25)
            uniform token[] xformOpOrder = ["xformOp:scale"]
            color3f[] primvars:displayColor = [(0.1, 0.6, 1.0)]
        }
    }

    def Xform "SharedRoll" (prepend apiSchemas = ["IdtxBehaviorAPI"])
    {
        double3 xformOp:translate = (1.8, 1.0, -2.5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        token idtx:behavior:kind = "runtime"
        token idtx:behavior:scope = "entity"
        bool idtx:behavior:enabled = true
        string idtx:behavior:name = "shared_roll"
        string idtx:behavior:version = "0.1.0"
        string idtx:behavior:uuid = "handbook-shared-roll-v1"
        asset idtx:behavior:source = @../behaviors/shared-roll/behavior.rs@
        uniform token[] idtx:behavior:capabilities = ["scene_query", "stage_edit"]

        def Cube "Body"
        {
            double size = 1
            double3 xformOp:scale = (0.7, 0.15, 0.25)
            uniform token[] xformOpOrder = ["xformOp:scale"]
            color3f[] primvars:displayColor = [(1.0, 0.55, 0.1)]
        }
    }
}
```
